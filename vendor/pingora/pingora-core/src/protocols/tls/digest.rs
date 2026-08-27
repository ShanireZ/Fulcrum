// Copyright 2026 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! TLS information from the TLS connection

use std::any::Any;
use std::borrow::Cow;
use std::sync::Arc;

/// The TLS connection information
#[derive(Clone, Debug)]
pub struct SslDigest {
    /// The cipher used
    pub cipher: Cow<'static, str>,
    /// The TLS version of this connection
    pub version: Cow<'static, str>,
    /// The organization of the peer's certificate
    pub organization: Option<String>,
    /// The serial number of the peer's certificate
    pub serial_number: Option<String>,
    /// The digest of the peer's certificate
    pub cert_digest: Vec<u8>,
    /// The user-defined TLS data
    pub extension: SslDigestExtension,
    /// ★ 枢衡改动 14：客户端在 ClientHello 里发的 **SNI**（小写）。`None` = 它没发。
    ///
    /// ⚠ 上游没有这一格，而它是访问日志契约里的 `tls_sni`。
    /// 见 `vendor/pingora/FORK.md` 改动 14。
    pub sni: Option<String>,
    /// ★ 枢衡改动 14：**协商出来的 ALPN**（`h2` / `http/1.1` / `acme-tls/1`）。
    /// `None` = 客户端没提供 ALPN，或者没协商出交集。
    pub alpn: Option<String>,
}

impl SslDigest {
    /// Create a new SslDigest
    pub fn new<S>(
        cipher: S,
        version: S,
        organization: Option<String>,
        serial_number: Option<String>,
        cert_digest: Vec<u8>,
    ) -> Self
    where
        S: Into<Cow<'static, str>>,
    {
        SslDigest {
            cipher: cipher.into(),
            version: version.into(),
            organization,
            serial_number,
            cert_digest,
            extension: SslDigestExtension::default(),
            // ★ 枢衡改动 14：**有意不进 `new()` 的签名** —— 那样 rustls / s2n 两个
            //   后端的调用点一个字都不用改，而它们本来就填不出这两格。
            //   ⇒ 由各后端的 `from_ssl()` 在建好之后按字段赋值。
            sni: None,
            alpn: None,
        }
    }
}

/// The user-defined TLS data
#[derive(Clone, Debug, Default)]
pub struct SslDigestExtension {
    value: Option<Arc<dyn Any + Send + Sync>>,
}

impl SslDigestExtension {
    /// Retrieves a reference to the user-defined TLS data if it matches the specified type.
    ///
    /// Returns `None` if no data has been set or if the data is not of type `T`.
    pub fn get<T>(&self) -> Option<&T>
    where
        T: Send + Sync + 'static,
    {
        self.value.as_ref().and_then(|v| v.downcast_ref::<T>())
    }

    #[allow(dead_code)]
    pub(crate) fn set(&mut self, value: Arc<dyn Any + Send + Sync>) {
        self.value = Some(value);
    }
}
