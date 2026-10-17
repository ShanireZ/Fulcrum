#!/usr/bin/env bash
# shellcheck shell=bash
#
# 門禁那個 `target` 卷的兩件事：**它叫什麼**，以及**同一時刻只許一次門禁用它**。
#
# 三處 source 本檔案：`tests/m0/docker-run.sh` · `tests/m1/systemd-run.sh` · `tests/ci/cache.sh`。
#
# ★ ★ **為什麼是一個檔案而不是三份表達式。** 卷名此前在那三處各寫了一遍
#   `fulcrum-target-${SHA:0:12}`。三處各算各的，規則一改就會安靜地指向兩個不同的卷 ——
#   而失效形態不是回報錯誤：M1 會掛上一個**自動新建的空卷**，CI 快取會**永遠不命中**，
#   現象只是「怎麼找不到二進位」和「怎麼還是這麼慢」。`tests/ci/cache.sh` 頂部
#   那句「不自己再造一套卷名演算法」說的就是這件事，本檔案是它的實作。

# ── 這棵工作樹的短標籤 ────────────────────────────────────────────────────────
#
# ★ ★ ★ **卷名必須同時跟著「哪個鏡像」與「哪一棵工作樹」走。**
#
#   「跟著鏡像走」的理由在 `docker-run.sh` 裡寫著，仍然成立（cargo 的 fingerprint
#   不覆蓋 C 工具鏈）。今天多出來的是第二條：**一臺機器上現在有好幾棵工作樹**
#   （主樹加 `.claude/worktrees/` 下若干），而容器把各自的 `/w` 掛成不同的原始碼樹、
#   卻把同一個 `/w/target` 掛給了所有人。
#
#   ⚠ ⚠ 後果不是「編得慢」，是**門給出別人家的讀數，而兩邊都不紅**。實測兩種：
#     ① 一次編譯錯誤指向一個只存在於另一棵樹的符號；
#     ② 一次報「696 條全綠、結束代碼 0」，跑的卻是**沒有本次新判據**的舊測試二進位。
#   第二種是致命的那一種 —— 它不回報錯誤，它給一個像樣的、綠的、錯的答案。
#
# ★ 傳進來的路徑請一律是 `cd "$(dirname …)/../.." && pwd` 的結果（三處呼叫點都是這麼來的）。
#   本函式內部再按平台歸一一次，免得同一棵樹在兩個呼叫點算出兩個標籤。
#
# ★ 歸一單獨拆出來，因為**它有第二個使用者**：捲上那個記「屬於哪棵樹」的 label
#   （見下面 `fulcrum_target_vol_create`）。兩處各歸一各的話，label 裡的路徑與
#   卷名裡的雜湊會指向同一棵樹的兩種寫法，而症狀是回收提示把一棵活著的樹報成「已經不在」。
# ★ ★ ★ **同一棵樹在這臺宿主上有不止一個路徑。** Docker Desktop 把 WSL 側的 bind 源
#   暫存到 `<暂存根>/<发行版>/<sha256(源路径)>` 再掛進容器，而**那個暫存掛載不隨容器退出釋放**
#   （實測：`docker kill` 之後目錄與掛載都還在；這臺宿主上一度堆著 58 個指向同一棵樹的）。
#   ⇒ 一旦某一輪的 `pwd` 落在暫存路徑上，`fulcrum_tree_tag` 就會算出一個新標籤，
#     而它的 sha256 又正好是**下一層**暫存目錄的名字 —— 於是每跑一輪生一個新卷，
#     一個約 8.7GB，且**快取從此永不命中（每輪全量重編）**。
#   ⚠ ⚠ 失效形態裡沒有任何一行錯誤：門照跑、照綠，只是慢，且磁碟無聲地漲。
#   ⇒ 歸一的第一件事是**把暫存路徑反查回真實源**，鏈就形成不了。
#
# ★ 反查讀 `mountinfo`：暫存那一行給出「設備號 + 該掛載在原始檔系統裡的子路徑」，
#   同設備號裡 `root` 為 `/` 的那一行給出該檔案系統掛在哪 —— 兩者拼起來就是真實源。
# ⛔ **查不到就原樣回傳，絕不猜**：猜錯的代價是把兩棵樹併成一個卷，
#   而那正是本檔案開頭那條「門給出別人家的讀數、兩邊都不紅」。
FULCRUM_STAGING_ROOT=${FULCRUM_STAGING_ROOT:-/mnt/wsl/docker-desktop-bind-mounts}
FULCRUM_MOUNTINFO=${FULCRUM_MOUNTINFO:-/proc/self/mountinfo}

# MSYS（Git Bash）的安裝根，Windows 寫法。`fulcrum_tree_local` 用它認出
# 「被 cygpath 映到安裝根底下」的那一類外來 POSIX 路徑。⚠ 取不到就留空，
# 那一條判斷隨之整條跳過（寧可少攔，不可把每個路徑都攔成不明）。
FULCRUM_MSYS_ROOT=""
if command -v cygpath >/dev/null 2>&1; then
  FULCRUM_MSYS_ROOT=$(cygpath -m / 2>/dev/null || true)
  FULCRUM_MSYS_ROOT=${FULCRUM_MSYS_ROOT%/}
fi

fulcrum_mountinfo_real() {
  awk -v mp="$1" '
    $5 == mp   { dev = $3; root = $4 }
    $4 == "/"  { base[$3] = $5 }
    END {
      if (dev == "" || !(dev in base)) exit 1
      b = base[dev]
      if (root == "/") { print b;      exit 0 }
      if (b == "/")    { print root;   exit 0 }
      print b root
    }
  ' "$FULCRUM_MOUNTINFO" 2>/dev/null
}

# `$1` = 可能是暫存路徑的路徑。回傳真實源；不是暫存路徑、或反查不出，一律原樣回傳。
#
# ⚠ ⛔ **段數不能用 `case` 的 `*` 去數** —— bash 的 `*` 連 `/` 一起吃，
#   `"$根"/*/*` 會把 `根/a/b/c/d` 也認下來。這裡改用參數展開逐段剝。
fulcrum_tree_unstage() {
  local path=${1:-} sub distro tail hash rest real
  case "$path" in
    "$FULCRUM_STAGING_ROOT"/*) ;;
    *) printf '%s' "$path"; return 0 ;;
  esac
  sub=${path#"$FULCRUM_STAGING_ROOT"/}
  case "$sub" in */*) ;; *) printf '%s' "$path"; return 0 ;; esac
  distro=${sub%%/*}
  tail=${sub#*/}
  hash=${tail%%/*}
  rest=${tail#"$hash"}
  real=$(fulcrum_mountinfo_real "$FULCRUM_STAGING_ROOT/$distro/$hash") || real=
  [ -n "$real" ] || { printf '%s' "$path"; return 0; }
  printf '%s%s' "$real" "$rest"
}

# ── 規範形：一棵樹在這臺機器上只許有一個標籤 ────────────────────────────────
#
# ★ ★ ★ **同一棵樹在 WSL 與 Git Bash 裡是兩種寫法**（`/mnt/d/W/F` 與 `/d/W/F`），
#   而標籤是寫法的雜湊 ⇒ 同一棵樹在兩個 shell 下拿到**兩個卷**。
#   ⚠ 這一條不像上面那條會越滾越多，但它是常駐的：一棵樹兩份快取，滿載各 8–10GB。
#   ⛔ 它**不是**本檔案頂部那條「門給出別人家讀數」——那條要求兩棵**不同**的樹共用
#     一個卷，這裡是反過來（一棵樹兩個卷）：浪費，但讀數是對的。
#
# ⇒ 規範形取 **Windows 盤符式 `X:/…`（大寫盤符）**。兩條理由：
#   ① Git Bash 的 `cygpath -m` 本來就產出這一形，改動面最小；
#   ② 換過來那一刻，**已經暖著的那個卷正好是它**，冷編代價接近零（2026-09-05 實測）。
#
# ⚠ ⚠ 規範形是給**標籤與 label** 用的，判活不能直接拿它去 `[ -d ]` ——
#   WSL 吃不下 `D:/…`。⇒ 必須配一個反向的 `fulcrum_tree_local`，
#   否則 label 全變成 Windows 寫法之後，**WSL 那邊的回收路會整條死掉**（判什麼都是 unknown）。
fulcrum_tree_canon() {
  local p=${1:-} d letter rest
  case "$p" in
    # 反查不出的暫存路徑：原樣留著，讓呼叫方按「不明」處理
    "$FULCRUM_STAGING_ROOT"|"$FULCRUM_STAGING_ROOT"/*) printf '%s' "$p"; return 0 ;;
    /mnt/[A-Za-z]|/mnt/[A-Za-z]/*)
      d=${p#/mnt/}
      letter=${d%%/*}
      rest=${d#"$letter"}
      printf '%s:%s' "$(printf '%s' "$letter" | tr '[:lower:]' '[:upper:]')" "${rest:-/}"
      return 0 ;;
  esac
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -m "$p"
  else
    printf '%s' "$p"
  fi
}

# 規範形 → **本 shell 拿得去 `[ -d ]` 的寫法**。夠不到就打空串，⛔ 不猜。
#
# ★ 「夠不到」用空串表達，而不是再來一個布爾判斷：呼叫方只有一個分支要寫
#   （空 ⇒ unknown），而「沒能檢查」與「檢查過了，沒有」由此天然分開。
#
# ⚠ 殘餘風險，說在明處：Git Bash 裡一個 **WSL 原生**（不在 `/mnt/<盘符>/` 下，
#   例如 `/home/u/tree`）的 label，`cygpath` 會把它映到 MSYS 安裝根底下 ——
#   那個路徑不存在，於是判 `gone`。這裡靠「映到 MSYS 根底下 ⇒ 夠不到」把它攔成 unknown。
#   ⛔ 別把這條讀成「所有 POSIX 路徑都攔」：Git Bash 自己的 `/tmp/x` 映到
#     Windows 的暫存目錄（**不在** MSYS 根底下，實測），它照樣判得動。
fulcrum_tree_local() {
  local p letter rest
  p="$(fulcrum_tree_canon "${1:-}")"
  if command -v cygpath >/dev/null 2>&1; then
    if [ -n "$FULCRUM_MSYS_ROOT" ]; then
      case "$p" in "$FULCRUM_MSYS_ROOT"/*) printf ''; return 0 ;; esac
    fi
    printf '%s' "$p"
    return 0
  fi
  case "$p" in
    [A-Za-z]:|[A-Za-z]:/*)
      letter=$(printf '%s' "${p%%:*}" | tr '[:upper:]' '[:lower:]')
      rest=${p#*:}
      printf '/mnt/%s%s' "$letter" "${rest:-/}" ;;
    /*) printf '%s' "$p" ;;
    *)  printf '' ;;
  esac
}

fulcrum_tree_norm() {
  fulcrum_tree_canon "$(fulcrum_tree_unstage "${1:-}")"
}

fulcrum_tree_tag() {
  local path=$1 norm
  norm="$(fulcrum_tree_norm "$path")"
  if command -v sha256sum >/dev/null 2>&1; then
    printf '%s' "$norm" | sha256sum | cut -c1-12
  elif command -v git >/dev/null 2>&1; then
    printf '%s' "$norm" | git hash-object --stdin | cut -c1-12
  else
    # ★ ★ 備援**不許退化成一個常數** —— 那正好把「兩棵樹共用一個卷」這件事重新造出來。
    #   退而求其次：把路徑本身洗成合法的卷名字元，它照樣是一樹一個，只是不好看。
    printf '%s' "$norm" | tr -c 'a-zA-Z0-9' '_' | tail -c 40
  fi
}

# `$1` = Dockerfile 的內容雜湊（可為空）；`$2` = 工作樹路徑。
#
# ★ 順序是「鏡像雜湊在前、樹標籤在後」，因為 `docker-run.sh` 要按後綴
#   `-<树标签>` 把「本樹的舊卷」與「別的樹的卷」分開 —— 前者可以勸人刪，後者絕不可以。
fulcrum_target_vol() {
  printf 'fulcrum-target-%s-%s' "${1:0:12}" "$(fulcrum_tree_tag "$2")"
}

# ── 卷屬於哪棵樹：寫在 label 上 ──────────────────────────────────────────────
#
# ★ ★ **「別的樹的卷絕不勸人刪」解決了一半，另一半是磁碟。** 工作樹是會消失的
#   （`.claude/worktrees/` 下那些用完就刪），而它們的卷不會跟著消失 —— 一個約 6GB，
#   躺在那裡，**沒有任何東西說得出它還有沒有主人**：卷名後綴是雜湊，反推不迴路徑。
#   ⇒ 新建時把歸一後的樹路徑記在 label 上，回收提示據此把「主人已經不在了」的那些
#     單列出來。這一條**不放寬** `docker-run.sh` 那條規矩：還在的樹，照舊不給刪除命令。
#
# ⚠ label **只有新建那一次寫得進去**：對一個已存在的卷，`docker volume create`
#   是**靜默的 no-op** —— 不回報錯誤，也不更新 label（實測）。這不構成問題，因為樹變了
#   卷名就變了 ⇒ 名字與 label 永遠是同一次新建寫下的，不可能各說各話。
#   ★ 反過來說，**沒有 label 的卷一定不是這條規則建的**（加它之前留下的，或手動建的），
#     那一類只能報「不明」，絕不能算成「已經不在」。
FULCRUM_TREE_LABEL="cool.cnb.fulcrum.tree"

# `$1` = 卷名；`$2` = 工作樹路徑（原樣傳，內部歸一）。
#
# ⚠ 記的是 `fulcrum_tree_norm` 歸一之後那一份，而不是 `/d/…` 那種 MSYS 路徑：
#   後者會被 Git Bash 在傳給 docker.exe 時改寫成 `D:/…`（實測），於是同一棵樹
#   在不同呼叫點記出兩種寫法。歸一之後本來就是 `D:/…`，沒有可改寫的東西。
fulcrum_target_vol_create() {
  docker volume create --label "${FULCRUM_TREE_LABEL}=$(fulcrum_tree_norm "$2")" "$1" >/dev/null
}

# 一個 label 值對應的樹現在是什麼狀態：live / gone / unknown。
#
# ★ `unknown` 與 `gone` **必須分開**：前者是「沒能檢查」，後者是「檢查過了，沒有」。
#   混成一個的話，加這條規則之前留下的卷會被當成無主的報出去。
# ★ ★ ★ **`[ -d ]` 只回答「本 shell 找不找得到這個字串」，不回答「那棵樹在不在」。**
#   label 是**建卷那一臺 shell** 的寫法，而回收提示可能在另一臺 shell 裡跑：
#   實測同一個卷、同一份判據，WSL 裡判 `live`，Git Bash 裡判 `gone` ——
#   因為 `/mnt/d/WorkSpace/Fulcrum` 在 Git Bash 裡就是不存在。
#   ⚠ ⚠ 判成 `gone` 的後果正是本函式上面那條自測點名要防的：**回收提示會給出
#     一條刪掉活著的快取的命令**，而受害者那邊只看到一次莫名的全量重編。
#   ⇒ 看不懂的寫法一律 `unknown`。⛔ 少回收一個卷只是佔盤，刪錯一個是別人半小時。
#
# ★ 先反查暫存路徑：暫存掛載還在時它解得回真實樹，判得準；解不回來就落 `unknown`，
#   ⛔ **不拿「那個暫存目錄還在不在」當「那棵樹在不在」** —— 它答的是另一個問題，
#   而且答案隨 Docker Desktop 有沒有重新啟動過而變（實測暫存掛載不隨容器退出釋放）。
fulcrum_tree_state() {
  local tree=${1:-} real here
  [ -n "$tree" ] || { printf 'unknown'; return 0; }
  real="$(fulcrum_tree_unstage "$tree")"
  case "$real" in
    "$FULCRUM_STAGING_ROOT"/*) printf 'unknown'; return 0 ;;   # 反查不出，別猜
  esac
  here="$(fulcrum_tree_local "$real")"
  if [ -z "$here" ]; then
    printf 'unknown'          # 本 shell 夠不到這種寫法 ——「沒能檢查」不算「檢查通過」
  elif [ -d "$here" ]; then
    printf 'live'
  else
    printf 'gone'
  fi
}

# ── 第二條判據：這個卷名，**今天這份推導造得出來嗎** ──────────────────────────
#
# ★ ★ ★ **為什麼 label 那一條不夠。** label 記在建卷那一刻，而對一個已存在的卷
#   `docker volume create` 是**靜默 no-op** ⇒ **機制存在之前留下的卷永遠補不上 label**，
#   一律落進「不明」，而「不明」這一半隻進不出 —— 它沒有任何出口。
#   ⚠ ⚠ 這條缺口的代價，2026-09-04 在**兩臺開發機上各量過一次**：
#     · 開發機 A（寫下這段註解的那臺）：11 個 target 卷、10 個無主、約 92GB，
#       回收提示**看得見 1 個**。三個成因彼此無關：舊命名代 8 個 · label 鍵還沒統一
#       那一代 1 個（鍵寫成了 `…checkout`）· 本樹自己的卷比 label 機制早生 17 分鐘。
#     · 開發機 B（`ShanireHomePC`）：12 個 `fulcrum*` 卷、6 個無主、約 48GB，
#       六個**全部**落在下面的規則 ③（建於 label 機制之前，`Labels` 是空的 `map[]`）。
#   ★ ★ **並排放這兩行不是記流水賬，是這段註解的判據本身**：卷是**機器本機狀態**，
#     ⇒ 「本機」「實測」這類詞在原始碼註解裡沒有機器名時，讀的人無從分辨它說的是哪一臺。
#     ⛔ 換臺機器讀到這裡，別把上面任何一個數當成你面前這臺的讀數 —— 自己數一遍。
#   ⇒ 缺的不是「多讀一個鍵」，是一條**不相依建卷那一刻記下了什麼**的判據。
#
# ★ 本判據不問「它是什麼時候建的」，問的是 `fulcrum_target_vol` 今天**造不造得出它**。
#   造不出來 ⇒ 三個呼叫點（`docker-run.sh` / `systemd-run.sh` / `cache.sh` 都走這一份推導）
#   沒有任何一條夠得著它。⛔ 這不是「看起來很舊」那種年齡啟發 —— 年齡是啟發，
#   「現行程式碼造不出這個名字」是關於**今天這份程式碼**的陳述，讀一份推導就能證。
#
# ⚠ 誠實說出殘餘風險：安全性建立在「夠得著它的只有這三處」之上。若有人在另一棵工作樹上
#   checkout 著舊提交跑舊指令碼，它的卷會是舊代形狀 ⇒ 會被列進清單。後果是**那棵樹下次
#   全量重編**（一次冷編），不是資料丟失；而 `docker volume rm` 對佔用中的卷本來就會拒絕。
#
# ⚠ ⚠ 下面這個正則是**手寫**的，而手寫的形狀會隨命名方案改動悄悄失效。兩道東西釘住它：
#   ① `selftest_vol_shape` 拿 `fulcrum_target_vol` **真的生成一個名字**來比 ——
#      推導一改形狀，那條自測當場紅（⛔ 手寫一個範例名去比是釘不住的）；
#   ② `docker-run.sh` 每次跑再拿**本樹的真名字**自證一次 —— `fulcrum_tree_tag` 在沒有
#      sha256sum 也沒有 git 的宿主上會退到另一種標籤，那時本判據自己失效，
#      而**失效必須說出來**，絕不能默默把當代的卷判成舊代的。
fulcrum_vol_shape() {
  if [[ "${1:-}" =~ ^fulcrum-target-[0-9a-f]{12}-[0-9a-f]{12}$ ]]; then
    printf 'current'
  else
    printf 'legacy'
  fi
}

# 一個**不屬於本樹**的卷該怎麼處置：`orphan`（可以給刪除命令）/ `keep`（只報數）。
#
# `$1` = 卷名；`$2` = label 裡讀出來的樹路徑（可空）；`$3` = 形狀判據這一輪可不可信（1/0）。
#
# ★ 三條規則**有次序**，而次序本身是承重的：
#   ① label 說主人**還在** ⇒ 一律 `keep`。★ ★ 它必須**短路在最前面** —— 否則一個
#      「舊代形狀 + 樹還活著」的卷會掉進規則 A，而那就是「勸人刪掉隔壁正在編譯的快取」。
#      ⚠ 這個洞是寫反證時才發現的，⛔ 不是設計時就想到的。
#   ② label 說主人**已經不在** ⇒ `orphan`（原有的規則，一個字沒改）。
#   ③ 讀不到 label，**且**今天的推導造不出這個名字 ⇒ `orphan`。
#   其餘一律 `keep` —— 「沒能檢查」不算「檢查通過」。
#
# ⚠ ⚠ **規則 ③ 絕不許吞掉規則 ② 保守的那一半**：名字是**當代形狀**、但讀不到 label 的，
#   照舊 `keep`。放寬一次的症狀不是回報錯誤，是有一天它給出一條刪錯卷的命令。
fulcrum_vol_verdict() {
  local name=${1:-} tree=${2:-} shape_ok=${3:-0} state
  state=$(fulcrum_tree_state "$tree")
  if [ "$state" = live ]; then
    printf 'keep'
  elif [ "$state" = gone ]; then
    printf 'orphan'
  elif [ "$shape_ok" = 1 ] && [ "$(fulcrum_vol_shape "$name")" = legacy ]; then
    printf 'orphan'
  else
    printf 'keep'
  fi
}

# 一批卷各自佔多少盤，一行一個 `名字|体积`。
#
# ⛔ **有意不求和。** `docker system df` 給的是**已經格式化過的字串**（`9.628GB`），
#   在 bash 裡解析單位再相加是一臺會給出「像樣的錯答案」的一次性量具，而本儲存庫剛在
#   別處栽過這一跤。逐個列出體積同樣達到目的：**讓代價說得出口**。
#   ⇒ 「另有 10 個卷」讀起來無關痛癢，「另有 10 個卷，9.6GB / 16.7GB / 11.1GB …」不是。
#
# ⚠ 量不到就一個字都不打，由呼叫方說「沒量到」——「讀不到」不許變成「沒這回事」。
# ★ 它比 `docker volume ls` 貴得多（開發機 A 上實測 1.4–1.9s vs 0.34s；⚠ 這個成本隨**宿主上所有卷**
#   的內容漲，不只隨 fulcrum 的卷漲 ⇒ 換臺機器要重新量，別用這個數）⇒ 呼叫方只在
#   真的有東西要報的時候才調它，桶是空的就一次都不調。
fulcrum_vol_sizes() {
  docker system df -v --format '{{range .Volumes}}{{.Name}}|{{.Size}}
{{end}}' 2>/dev/null || true
}

# 從 `fulcrum_vol_sizes` 的輸出裡取一個卷的體積。`$1` = 那份文字（可為空）；`$2` = 卷名。
#
# ★ 單獨成函式是因為它有**兩個**呼叫點（回收提示的第二、第三個桶各一個），
#   而本檔案頂部那條「為什麼是一個檔案而不是三份表達式」講的正是這件事：
#   各寫一遍的失效形態不是回報錯誤，是有一天兩邊對「沒量到」給出不同的說法。
# ⚠ 量不到時打「未量到」，⛔ 絕不打 0 ——「讀不到」不許變成「沒這回事」。
# ★ 逐欄位比**全等**，不是子串：`grep -F "$want|"` 那種寫法會讓 `x-fulcrum-cargo|…`
#   這一行被 `fulcrum-cargo` 認領，於是一個卷被報上另一個卷的體積。
fulcrum_vol_size_of() {
  local blob=${1:-} want=${2:-} n s
  if [ -n "$blob" ]; then
    while IFS='|' read -r n s; do
      if [ "$n" = "$want" ]; then printf '%s' "$s"; return 0; fi
    done <<< "$blob"
  fi
  printf '未量到'
  return 0
}

# ── 第三條判據：這臺宿主上還有哪些卷，本門**既管不著、又說不出屬於誰** ──────
#
# ★ ★ ★ **為什麼需要第三個桶。** 上面兩條判據都只在**掃描集內**說話，而掃描集是
#   `docker volume ls -q --filter name=fulcrum-target` —— 一個**子串**篩子。
#   2026-09-04 / 09-05 兩天裡逃出這個篩子的卷有三族，**三族全部是人工發現的**，
#   門禁一次都沒幫上忙：
#     · `fulcrum-upstream-target`（投稿六那棵上游克隆臨時建的）—— 不含 `fulcrum-target` 子串
#     · `fulcrum-musl-{probe,alpine}-{target,cargo}` ×4（musl 探針那場）—— 同上
#     · `pingora-up994-target` 10.51GB —— **連 `fulcrum` 都不含**，無 filter 列一遍才看見
#   ⇒ 共同成因**不是「篩子太窄」**：篩子按**名字**判，而要找的東西是「本儲存庫的活造出來的」，
#     名字只是屬主的代理。⛔ 因此「放寬到 `name=fulcrum-`」治不了第三族 ——
#     那條 2026-09-04 擺出來的候選，2026-09-05 被第三族證偽了。
#
# ★ ★ ★ 於是判據換個問法：**屬主判不了，能判的只有「報得多寬」。**
#   本判據不刪任何東西、不給任何刪除命令，它只回答一句：
#   「這臺宿主上有哪些卷，本門既管不著、又說不出屬於誰」。
#   ⚠ ⚠ 它的失效方向是**噪音**（白名單過期 ⇒ 多報一行），不是**沉默**（漏報一個卷）——
#     這是選它、而不是繼續放寬 filter 的**全部理由**。
#
# 別的專案在這臺宿主上的卷，**按前綴**匹配，逐條註明屬主。
#
# ⚠ ⚠ 這份白名單本身就是一個新篩子 ⇒ 它有變成**消音手段**的風險。三條紀律：
#   ① 只放**別的專案**的卷，逐條寫出屬主；⛔ 不許為了讓某一行別再出現而往裡加。
#   ② 前綴**錨在開頭**，⛔ 不是子串 —— 子串正是本條要修的那個缺陷本身。
#   ③ 下面的自測釘住：白名單不許吞掉任何 `fulcrum` 開頭的名字。
#   ★ ★ 誠實說出它守不住什麼：**它攔不住有人把一個真正屬於本儲存庫、名字裡卻沒有 `fulcrum`
#     的卷（`pingora-up994-target` 就是這個形狀）寫進白名單。** 那一格沒有門，只有紀律。
# ⚠ 這份清單是**某一臺開發機上**的讀數，換臺機器多半要加/減 —— 加不上沒關係，
#   後果只是多報幾行（安全方向）。⛔ 別為了讓它「乾淨」而刪掉本機用不到的條目。
FULCRUM_FOREIGN_VOL_PREFIXES='act-toolcache nektos/act 的工具缓存（Node 26.7.0）——★ 属主是「用 act 跑 CI 的那几个 Node 项目」而不是某一个：这个名字由 act 自己起、不带项目前缀，本机 26.7.0 对得上 BetAI/ChengJun/LuoguSP/WenTian 四家。⛔ 不是本仓的（gate.yml 明写本仓没有 Node 代码、不用 setup-node），2026-09-07 按三条判据逐条查过
betai-      betai 教学平台
betapass    BetaPass 身份中枢
nxtstrict-  Plumbline 的 nxtstrict 探针
plumbline-  Plumbline
wentian     WenTian'

# `$1` = 卷名。三選一：`anonymous` / `foreign` / `unattributed`。
#
# ⛔ 它**不**回答「是不是本儲存庫的」—— 那件事只有呼叫方說得準（只有呼叫方知道
#   `TARGET_VOL` 與掃描集），所以本儲存庫自己的卷由呼叫方按精確名字先摘掉。
fulcrum_vol_attribution() {
  local name=${1:-} prefix
  # docker 自己建的匿名卷：名字恰好 64 位十六進制。
  # ⚠ 長度寫死是**有意**的：63 位或 65 位都不是 docker 的匿名卷，而是有人起的名字。
  #   放寬成「一串十六進制」會把那些靜默吞掉，而吞掉的方向正是本判據要防的沉默。
  if [[ "$name" =~ ^[0-9a-f]{64}$ ]]; then printf 'anonymous'; return 0; fi
  while read -r prefix _; do
    [ -n "$prefix" ] || continue
    case "$name" in
      "$prefix"*) printf 'foreign'; return 0 ;;
    esac
  done <<< "$FULCRUM_FOREIGN_VOL_PREFIXES"
  printf 'unattributed'
  return 0
}

# `$1` = 全機卷名（一行一個，`docker volume ls -q` 的原樣輸出）
# `$2` = 本門管轄內的卷名（一行一個：本樹活卷、有意共享的 cargo 卷、掃描集裡的全部）
# 輸出：本門管不著、又說不出屬主的那些，一行一個（可能一行都沒有）。
#
# ★ ★ 把這一段做成函式、而不是寫在 `docker-run.sh` 的循環裡，是為了讓它**能被自測**：
#   判據函式對了、而循環裡一個 `continue` 放錯位置，症狀同樣是「一行都不打」——
#   與桶真的是空的長得一模一樣。⇒ 兩個方向都由 `selftest_unattributed_vols` 釘住。
fulcrum_unattributed_vols() {
  local all=${1:-} scope=${2:-} v
  while read -r v; do
    [ -n "$v" ] || continue
    if printf '%s\n' "$scope" | grep -qxF -- "$v"; then continue; fi
    if [ "$(fulcrum_vol_attribution "$v")" = unattributed ]; then printf '%s\n' "$v"; fi
  done <<< "$all"
  return 0
}

# ── 門禁互斥 ────────────────────────────────────────────────────────────────
#
# ★ ★ ★ **卷分開了還不夠**：同一棵樹上並行跑兩次門禁，兩次仍然共用那一個卷，
#   於是回到同一種失效 —— 一次跑讀到另一次跑寫了一半的產物，而兩邊都不紅。
#
# ★ ★ **用 `mkdir` 而不是 `flock`，因為這臺宿主上沒有 `flock`。**
#   宿主是 Windows + Git Bash（MSYS），`command -v flock` 空手而歸（實測）。
#   寫一把在這臺機器上其實不生效的鎖，比沒有鎖更壞 —— 它會讓人以為並行已經攔住了。
#   `mkdir` 在目錄已存在時必然失敗、且不留副作用，這一點在 NTFS 與 ext4 上都成立。
#   ⚠ 絕不可以退化成 `[ -d "$dir" ] || mkdir "$dir"` —— 那是兩步，中間正好是競態視窗。
#
# 鎖放在宿主機的暫存目錄裡，不放儲存庫裡：儲存庫裡那份會被「target/ 必須為空」和行尾兩道門看見。
FULCRUM_LOCK_ROOT=${FULCRUM_LOCK_ROOT:-${TMPDIR:-/tmp}/fulcrum-gate-locks}
FULCRUM_LOCK_DIR=${FULCRUM_LOCK_DIR:-}   # 本處理程序真正持有的那一把（空 = 沒持有）

# `$1` = 鎖名（＝卷名）；`$2` = 說明用的工作樹路徑。拿不到回傳 1，**不排隊、不等待**。
fulcrum_lock_acquire() {
  local name=$1 tree=${2:-<未知>} dir owner
  dir="$FULCRUM_LOCK_ROOT/$name.lock"

  # ★ 可重入，靠一個匯出的環境變數而**不是**比 pid：`docker-run.sh` 在自己持鎖期間會調
  #   `tests/m1/systemd-run.sh`，而後者單獨跑時必須自己上鎖。比 pid 的話，
  #   「同一臺機器上另一次門禁」也會被認成自己人。
  if [ "${FULCRUM_GATE_LOCK_HELD:-}" = "$dir" ]; then
    return 0
  fi

  mkdir -p "$FULCRUM_LOCK_ROOT"

  if ! mkdir "$dir" 2>/dev/null; then
    owner=$(cat "$dir/pid" 2>/dev/null || true)
    if [ -n "$owner" ] && kill -0 "$owner" 2>/dev/null; then
      echo "★ 另一次门禁正在这棵工作树上跑，本次不启动。" >&2
      echo "    树   ：$tree" >&2
      echo "    卷   ：$name" >&2
      echo "    持有 ：pid $owner，自 $(cat "$dir/since" 2>/dev/null || echo '未知时间')" >&2
      echo "  ⚠ 两次门禁共用同一个 target 卷，一起跑会互相读到对方写了一半的产物 ——" >&2
      echo "    而那种坏法不报错，它给一个像样的、绿的、错的答案。" >&2
      echo "  等那一次跑完再来；确认它其实已经死了就删掉锁目录：" >&2
      echo "      rm -rf \"$dir\"" >&2
      return 1
    fi
    # ★ 持有者已經不在了（上一次被 Ctrl-C / 被 kill，沒走到 trap）——**接管，並且說出來**。
    #   一次沒人知道發生過的接管，與沒有鎖無異。
    # ⚠ 殘留風險說在明處：pid 會被系統重複使用，理論上可能把一把活鎖誤判成陳舊鎖。
    #   代價對比很清楚 —— 不接管的話，一次 Ctrl-C 就讓這棵樹上的門禁**永久**起不來。
    echo "⚠ 这棵树上留着一把没人认领的门禁锁（pid ${owner:-<未知>} 已不在），本次接管。" >&2
    echo "    $dir" >&2
    rm -rf "$dir"
    mkdir "$dir" 2>/dev/null || {
      echo "★ 接管失败：同一瞬间有第三方也在抢这把锁。重跑一次即可。" >&2
      return 1
    }
  fi

  printf '%s\n' "$$" > "$dir/pid"
  printf '%s\n' "$tree" > "$dir/tree"
  date '+%Y-%m-%dT%H:%M:%S%z' > "$dir/since" 2>/dev/null || true
  FULCRUM_LOCK_DIR="$dir"
  export FULCRUM_GATE_LOCK_HELD="$dir"
  # ★ trap 在這裡設，不留給呼叫方：這類鎖最常見的壞法就是「忘了設 trap」，
  #   而那種壞法要等到下一次跑不起來才被發現。
  #   ⚠ 兩個呼叫點原本都沒有 EXIT trap（已核對），所以這裡不會覆蓋掉別人的。
  trap fulcrum_lock_release EXIT
  return 0
}

# EXIT trap 調它。★ **只刪自己那一把**：接管別人的鎖是上面那條明確的路，
#   而在這裡誤刪別人的鎖會讓並行保護整個失效，且沒有任何症狀。
fulcrum_lock_release() {
  local held=${FULCRUM_LOCK_DIR:-}
  [ -n "$held" ] || return 0
  if [ "$(cat "$held/pid" 2>/dev/null || true)" = "$$" ]; then
    rm -rf "$held"
  fi
  FULCRUM_LOCK_DIR=""
  unset FULCRUM_GATE_LOCK_HELD
  return 0
}
