#!/usr/bin/env bash
# CI 构建缓存：把门禁用的两个 docker 命名卷导出成一个压缩流 / 再灌回去。
#
#   bash tests/ci/cache.sh save    <缓存目录>
#   bash tests/ci/cache.sh restore <缓存目录>
#
# ★ ★ ★ **为什么值得做，以及它的危险在哪**
#
#   实测（本机）：`fulcrum-cargo` 684MB、`fulcrum-target-*` **6.3GB**
#   （vendor 3.0G + debug 2.8G + release 580M）。冷缓存下一次全量 CI 要 17–21 分钟，
#   其中绝大部分是这两坨东西从零编出来。
#
#   ⚠ ⚠ **而缓存最坏的失效方式不是「没命中」，是「命中了一份不该用的」** ——
#   本仓库为此栽过一次同族的：`fulcrum-build:local` 里跑的一直是 bookworm，
#   而 Dockerfile 早就换成了 trixie，**此后每一次「全绿」用的基础镜像都不是仓库声明的那个**。
#   ⇒ 这个脚本的重点不是压缩，是**证明灌回去的那份配得上现在这套工具链**：
#     · 卷名本身带着 `Dockerfile.build` 的内容哈希（docker-run.sh 定的规矩，这里照抄同一套算法）；
#     · 归档里另存一份 `meta.txt`，**灌回之前逐字比对**，对不上就**跳过并说明**，绝不硬灌。
#
# ★ 不自己再造一套卷名算法：从 `docker-run.sh` 那条规则抄同一份哈希来源（同一个文件、同一段哈希）。
#   ⚠ 两处各算各的话，它们会在某次改动后安静地指向两个不同的卷 —— 那时缓存永远不命中，
#     而现象只是「CI 怎么还是这么慢」。
set -euo pipefail

MODE=${1:-}
DIR=${2:-}
if [ -z "$MODE" ] || [ -z "$DIR" ]; then
  echo "用法：bash tests/ci/cache.sh {save|restore} <缓存目录>" >&2
  exit 2
fi

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DOCKERFILE="$REPO/docker/Dockerfile.build"
[ -f "$DOCKERFILE" ] || { echo "找不到 $DOCKERFILE" >&2; exit 1; }

DOCKERFILE_SHA=$(sha256sum "$DOCKERFILE" | cut -d' ' -f1)
TARGET_VOL="fulcrum-target-${DOCKERFILE_SHA:0:12}"
CARGO_VOL=fulcrum-cargo

# ★ 搬运用的镜像**不另外钉一个**：直接从 `docker/Dockerfile.systemd` 的 FROM 行读回来。
#   那一行本来就是按 digest 钉死的，而且那个镜像后面跑 M1 场景时无论如何都要拉。
#   ⇒ 同一个事实只有一个来源，也不多拉一个镜像层。
HELPER=$(sed -n 's/^FROM \(debian:[^ ]*\).*/\1/p' "$REPO/docker/Dockerfile.systemd" | head -1)
[ -n "$HELPER" ] || { echo "从 Dockerfile.systemd 里读不出 FROM 行 —— 搬运镜像无从确定" >&2; exit 1; }

# ★ 扩展名有意**不写死成 `.zst`**：压缩器是按「这台机器上有什么」选的，
#   而一个叫 `.zst` 的 gzip 文件是一句会误导人的谎。
ARCHIVE="$DIR/fulcrum-build-cache.tar.z"
META="$DIR/meta.txt"

# 压缩器：有 zstd 就用（多线程、快），没有就退回 gzip，并**说出来**（不是静默降级）。
if command -v zstd >/dev/null 2>&1; then
  CODEC=zstd
  COMPRESS=(zstd -3 -T0 -q)
else
  CODEC=gzip
  [ "$MODE" = "save" ] && echo "⚠ 没有 zstd，退回 gzip（慢，但结果一样）"
  COMPRESS=(gzip -1 -c)
fi

# ★ ★ **解压按归档自己的魔数选，不按「这台机器上装了什么」选。**
#   ⚠ 存的那台有 zstd、取的那台没有（或反过来）是完全可能的，
#   而那时按环境猜会拿 `zstd -d` 去解一个 gzip 流 —— 报错跟真正的原因毫无关系。
#   ★ 判据挂在**产物**上，不挂在环境上 —— 与本仓库那几道「自证」是同一条纪律。
sniff_codec() {
  local magic
  # ⚠ `tr -d '[:space:]'` 而不是 `tr -d ' \n'`：后者里那个反斜杠在本仓库的
  #   工具链上被吃过好几次（heredoc 会剥掉一层），而剥掉之后它删的是字母 n ——
  #   十六进制里没有 n，所以**它照样能跑**，只是理由变了。能不写反斜杠就不写。
  magic=$(head -c 4 "$1" | od -An -tx1 | tr -d '[:space:]')
  case "$magic" in
    28b52ffd*) printf 'zstd' ;;
    1f8b*) printf 'gzip' ;;
    *) printf 'unknown' ;;
  esac
}

case "$MODE" in
  save)
    mkdir -p "$DIR"
    docker volume inspect "$TARGET_VOL" >/dev/null 2>&1 || {
      echo "★ 卷 $TARGET_VOL 不存在 —— 这一轮没什么可存的，跳过"
      exit 0
    }
    # meta 先写，再打包 —— 灌回时它是唯一的凭证。
    {
      echo "dockerfile-sha256=$DOCKERFILE_SHA"
      echo "target-volume=$TARGET_VOL"
      echo "codec=$CODEC"
      echo "saved-by=tests/ci/cache.sh"
    } > "$META"
    # ★ ★ ★ **半截归档必须自己清掉。** 实测（真发生了）：
    #   一次 `cancel-in-progress` 把门禁那一步掐了，导出这一步随之失败，
    #   **但半截的归档文件已经落盘**（370MB，完整的是 2.2GB），
    #   而 workflow 里那个「文件在不在」的判断照样成立 ⇒ 它把这份残档
    #   **存进了缓存键**。缓存条目不可变 ⇒ 此后每一轮都精确命中这个残档，
    #   而灌回时它会在解压到一半时炸掉。
    #   ⚠ ⚠ 也就是说：**「文件存在」不是「导出成功」** —— 判据要取动作的结果，
    #   不是取动作留下的痕迹。（这与本仓库那条「一行说它发生了的日志不是证据」同形。）
    # ★ 写成函数而不是内联 trap 字符串：内联那种写法里 `rc=$?` 在单引号里，
    #   那个扫查器看不见这次赋值（SC2154），而**为了让它闭嘴去加 disable 注释
    #   ⚠ 上一行有意不以工具名开头：`#` + 空白 + 那个名字会被当成 **directive** 解析，
    #     报 SC1072/SC1073。这条 `tests/vendor/run.sh` 里白纸黑字记着，我还是踩了。
    #   是最不该走的一条路** —— 换个它读得懂的写法就行。
    drop_partial_archive() {
      local rc=$?
      if [ "$rc" -ne 0 ]; then
        # ★ ★ ★ **先记大小，再删** —— 「它写到多大时爆的」是查这件事唯一的硬数字，
        #   而删掉之后就再也拿不到了。⚠ **这两行的前后顺序是取证的一部分，不是排版。**
        #   就是因为没有这一行，一次真实的 No-space 失败只留下了「失败了」三个字。
        if [ -f "$ARCHIVE" ]; then
          echo "★ 半截归档：$(du -b "$ARCHIVE" 2>/dev/null | cut -f1) 字节（$(du -h "$ARCHIVE" 2>/dev/null | cut -f1)）" >&2
        fi
        echo "★ 导出失败（退出码 $rc），清掉半截归档，绝不让它去认领缓存键" >&2
        rm -f "$ARCHIVE" "$META"
        # ★ ★ ★ **在这一刻量磁盘，而不是等回到 workflow 再量。**
        #   ⚠ ⚠ 的教训：workflow 里那次「导出后的磁盘」量到 `1.8G 可用 98%`，
        #   而同一轮换个时机量是 `32G 可用 56%` —— **`rm -f` 之后空间不一定立即释放**
        #   （写它的那个进程可能还持着 fd）。⇒ 那个「导出吃掉 28GB」的结论是一个
        #   **测量假象**，而它把整件事的方向带偏了好几轮。
        #   ★ 下面这几行在**失败的那一刻**量（`rm -f` 之前就已经取过归档大小了），
        #   问的才是「爆的时候到底什么样」。⚠ 它们只花毫秒。
        echo "★ 爆的那一刻 —— 归档所在的文件系统（★ 此前的盲区：以前只测过 /）：" >&2
        df -h "$DIR" >&2 2>&1 || true
        echo "★ 爆的那一刻 —— 根文件系统：" >&2
        df -h / >&2 2>&1 || true
        echo "★ 爆的那一刻 —— 内存与 swap（-T0 的 zstd 会吃内存，而 swap 在磁盘上）：" >&2
        # ⚠ Git Bash 上没有 `free`（那是 Linux 命令）。不测存在性的话，
        #   开发机上会冒一行 `command not found` —— 那看起来像脚本坏了。
        if command -v free >/dev/null 2>&1; then
          free -h >&2 2>&1 || true
        else
          echo "  （本机没有 free，这一项只在 Linux 上量得到）" >&2
        fi
        echo "★ 已知事实（CI 实测，不必每轮再量）：" >&2
        echo "  tar 流 9087109120 字节 ≈ 9.09GB；两个卷 352M + 8.2G —— 与 docker system df 对得上。" >&2
        echo "  ⇒ 卷与 tar 流都不是异常。异常在「31GB 可用，而只写了 1.8GB 就 No space」这一句上。" >&2
      fi
    }
    trap drop_partial_archive EXIT
    # ★ ★ **取证只留零代价的那几行。** 两个卷多大、tar 流多大，都已经量过且对得上
    #   （约 9GB，与 `docker system df` 一致）⇒ **卷与 tar 流都没有异常**。
    #   ⚠ ⚠ 而重新量一遍的代价是量出来的：两次各读 9GB，把导出这一步从
    #   **5m30s 拉到 24m54s**，整轮 CI 从 ~25 分钟涨到 **40m2s**。
    #   > **一个已经回答完的问题，不该每轮再问一遍。**
    #   ⚠ 而「只在失败路径上才跑」要真的做到 —— `du` 那种行写在外面就会每轮都跑。
    #
    # ★ ★ ★ **真正的缺口反倒被那一版量出来了，而它几乎不花钱**：
    #   `df -h /` 报**还有 31GB 可用**，而 zstd **只写了 1.8GB 就报 No space left**。
    #   ⇒ 说明 `df /` 与归档**实际写入的那个文件系统可能不是一个**
    #   （`$ARCHIVE` 在 `$DIR`，而此前只测过 `/`），也可能是 `-T0` 的 zstd 吃内存
    #   把系统推进 swap、或者某个 cgroup 限额。**没有证据之前不写结论。**
    echo "[cache] 归档要写到哪个文件系统上（★ 此前的盲区）："
    df -h "$DIR" 2>&1 || true
    echo "[cache] 导出 $CARGO_VOL + $TARGET_VOL → $ARCHIVE"
    # ★ tar 在容器里跑（卷只有容器够得着），压缩在**宿主机**上跑（runner 的 zstd 是多线程的）。
    #   ⚠ 一趟流式，不落 7GB 的中间文件 —— runner 的磁盘装不下那个中间文件。
    #
    # ★ ★ ★ **`--log-driver=none` 不是优化，它是这一整件事的根因。**
    #   查清：docker 默认的 **json-file 日志驱动**会把容器的 stdout
    #   **转义后写进 `/var/lib/docker/containers/<id>/<id>-json.log`** ——
    #   而这里的 stdout 恰好是**整个 9GB 的 tar 流**。
    #   ★ 本机实测（不带 `--rm` 跑一次再量）：7.83GB 的 tar 流 ⇒ 日志 **34,016,172,298 字节（31.7GB）**，
    #   膨胀约 **4 倍**。CI 上 tar 流 9.09GB ⇒ 日志 ≈ 26–36GB —— **正好就是那 28GB**。
    #   ⚠ ⚠ **`--rm` 救不了**：它只在容器**退出之后**删日志，
    #   而磁盘在它**还在跑**的时候就满了。
    #   ★ 这也解释了为什么「半截归档只有 1.8GB」与「磁盘涨了 28GB」同时成立 ——
    #   那 28GB 里只有 1.8GB 是归档，剩下的是一份没人要看的日志。
    # ⚠ ⚠ `MSYS_NO_PATHCONV=1` 在这一行上**不是可选的**：没有它时，Git Bash 会把
    #   `-C /` 里那个 `/` 改写成 `C:/Program Files/Git/` —— tar 当场报
    #   `Cannot open: No such file or directory`。★ CI 是 Linux，那里这个变量无害。
    #   ★ ★ 才发现：**在此之前 `save` 在开发机上根本跑不起来**，
    #   于是改动它只能靠一轮 CI（~20 分钟）去验 ——
    #   **而一个只能在 CI 上验的脚本，改它的人会候得不耐烦。**
    MSYS_NO_PATHCONV=1 docker run --rm --log-driver=none \
      -v "$CARGO_VOL:/c:ro" -v "$TARGET_VOL:/t:ro" "$HELPER" \
      tar -cf - -C / c t | "${COMPRESS[@]}" > "$ARCHIVE"
    echo "[cache] 归档大小：$(du -h "$ARCHIVE" | cut -f1)"
    ;;

  restore)
    if [ ! -f "$ARCHIVE" ]; then
      echo "[cache] 没有归档（$ARCHIVE）—— 冷启动，正常"
      exit 0
    fi
    # ★ ★ **灌回之前先验凭证。** 对不上就跳过，而且**说清楚为什么** ——
    #   静默硬灌正是「悄悄喂回陈旧产物」那条路。
    if [ ! -f "$META" ]; then
      echo "★ 有归档但没有 meta.txt —— 来路不明，**不灌**（这一轮按冷启动跑）"
      exit 0
    fi
    WANT="dockerfile-sha256=$DOCKERFILE_SHA"
    if ! grep -qxF "$WANT" "$META"; then
      echo "★ 归档不是这套工具链下产出的，**不灌**（这一轮按冷启动跑）："
      echo "    归档里：$(grep '^dockerfile-sha256=' "$META" || echo '（没有这一行）')"
      echo "    现在是：$WANT"
      exit 0
    fi
    FOUND=$(sniff_codec "$ARCHIVE")
    case "$FOUND" in
      zstd)
        command -v zstd >/dev/null 2>&1 || {
          echo "★ 归档是 zstd 压的，而这台机器上没有 zstd —— **不灌**（按冷启动跑）"
          exit 0
        }
        DECOMPRESS=(zstd -d -q -c)
        ;;
      gzip) DECOMPRESS=(gzip -d -c) ;;
      *)
        echo "★ 归档头四个字节既不是 zstd 也不是 gzip —— 来路不明，**不灌**（按冷启动跑）"
        exit 0
        ;;
    esac
    # ★ ★ ★ **碰卷之前先把归档从头到尾读一遍。**
    #   ⚠ 少了这一步，一份被截断的归档会**先往真卷里解出一半**再报错 ——
    #   而那一半是残缺的 .rlib，比「没有缓存」坏得多。
    #   真的出现过一份 370MB 的残档（完整的是 2.2GB）。
    #   ★ 两层都验：解压流验到底（`-t`），tar 也整个列一遍（`-tf`）——
    #     压缩流完整不代表里面那个 tar 完整。代价是多读一遍，换的是「绝不半灌」。
    echo "[cache] 灌回前先验一遍归档完整性（两层：压缩流 + tar）…"
    if ! "${DECOMPRESS[@]}" "$ARCHIVE" | tar -tf - > /dev/null 2>&1; then
      echo "★ 归档读不完整（截断／损坏）—— **不灌**，这一轮按冷启动跑。"
      echo "  ⚠ 一份残档比没有缓存坏得多：它会往卷里解出一半残缺的产物。"
      exit 0
    fi
    echo "[cache] 归档完整。灌回 $CARGO_VOL + $TARGET_VOL（$FOUND 压的）"
    docker volume create "$CARGO_VOL" >/dev/null
    docker volume create "$TARGET_VOL" >/dev/null
    "${DECOMPRESS[@]}" "$ARCHIVE" \
      | MSYS_NO_PATHCONV=1 docker run --rm -i --log-driver=none \
          -v "$CARGO_VOL:/c" -v "$TARGET_VOL:/t" "$HELPER" \
          tar -xf - -C /
    # ★ 灌完当场自证：卷里真的有东西了。
    #   ⚠ 少了这一条，一次「解包失败但退出码是 0」的灌回会让人以为缓存生效了，
    #     而实际上后面照样从零编 —— 现象只是「怎么还是这么慢」，没有任何一行会说出来。
    N=$(docker run --rm -v "$TARGET_VOL:/t:ro" "$HELPER" \
          sh -c 'find /t -maxdepth 1 -mindepth 1 | wc -l')
    if [ "${N:-0}" -gt 0 ]; then
      echo "[cache] 灌回后 $TARGET_VOL 顶层有 $N 项"
    else
      echo "★ 灌回之后卷还是空的 —— 缓存这一轮等于没有（不判红，但别以为它生效了）" >&2
    fi
    ;;

  *)
    echo "未知模式 $MODE（只认 save / restore）" >&2
    exit 2
    ;;
esac
