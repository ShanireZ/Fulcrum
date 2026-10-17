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

use crate::*;
use bytes::Bytes;
use http::header;

#[derive(Debug, Clone)]
pub struct CaseHeaderName(Bytes);

impl CaseHeaderName {
    pub fn new(name: String) -> Self {
        CaseHeaderName(name.into())
    }
}

impl CaseHeaderName {
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    pub fn from_slice(buf: &[u8]) -> Self {
        CaseHeaderName(Bytes::copy_from_slice(buf))
    }
}

/// A trait that converts into case-sensitive header names.
pub trait IntoCaseHeaderName {
    fn into_case_header_name(self) -> CaseHeaderName;
}

impl IntoCaseHeaderName for CaseHeaderName {
    fn into_case_header_name(self) -> CaseHeaderName {
        self
    }
}

impl IntoCaseHeaderName for String {
    fn into_case_header_name(self) -> CaseHeaderName {
        CaseHeaderName(self.into())
    }
}

impl IntoCaseHeaderName for &'static str {
    fn into_case_header_name(self) -> CaseHeaderName {
        CaseHeaderName(self.into())
    }
}

impl IntoCaseHeaderName for HeaderName {
    fn into_case_header_name(self) -> CaseHeaderName {
        CaseHeaderName(titled_header_name(&self))
    }
}

impl IntoCaseHeaderName for &HeaderName {
    fn into_case_header_name(self) -> CaseHeaderName {
        CaseHeaderName(titled_header_name(self))
    }
}

impl IntoCaseHeaderName for Bytes {
    fn into_case_header_name(self) -> CaseHeaderName {
        CaseHeaderName(self)
    }
}

fn titled_header_name(header_name: &HeaderName) -> Bytes {
    titled_header_name_str(header_name).map_or_else(
        || Bytes::copy_from_slice(header_name.as_str().as_bytes()),
        |s| Bytes::from_static(s.as_bytes()),
    )
}

/// The titled form a header name is written with on HTTP/1 when no case is recorded for it,
/// or `None` when it is written lowercase.
///
/// 樞衡改動 17（G162）：改成 `pub`（在 crate 根再匯出），讓呼叫方判得出「不建大小寫表時這個名會不會照原樣寫出」。
pub fn titled_header_name_str(header_name: &HeaderName) -> Option<&'static str> {
    Some(match *header_name {
        header::ACCEPT_RANGES => "Accept-Ranges",
        header::AGE => "Age",
        header::CACHE_CONTROL => "Cache-Control",
        header::CONNECTION => "Connection",
        header::CONTENT_TYPE => "Content-Type",
        header::CONTENT_ENCODING => "Content-Encoding",
        header::CONTENT_LENGTH => "Content-Length",
        header::DATE => "Date",
        header::TRANSFER_ENCODING => "Transfer-Encoding",
        header::HOST => "Host",
        header::SERVER => "Server",
        header::SET_COOKIE => "Set-Cookie",
        // 樞衡改動 17（G162）：樞衡自己構的回應會發、而上表沒有的那幾個。
        // ⚠ 這張表也決定用常數插入的名怎麼寫（有沒有大小寫表都一樣）⇒ 壓縮層改寫的 `ETag`、
        //   追加的 `Vary` 從此也照這裡寫，不再是小寫。
        header::ALLOW => "Allow",
        header::ALT_SVC => "Alt-Svc",
        header::CONTENT_RANGE => "Content-Range",
        header::ETAG => "ETag",
        header::LAST_MODIFIED => "Last-Modified",
        header::LOCATION => "Location",
        header::VARY => "Vary",
        // TODO: add more const header here to map to their titled case
        // TODO: automatically upper case the first letter?
        _ => {
            return None;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_case_header_name() {
        assert_eq!("FoO".into_case_header_name().as_slice(), b"FoO");
        assert_eq!("FoO".to_string().into_case_header_name().as_slice(), b"FoO");
        assert_eq!(header::SERVER.into_case_header_name().as_slice(), b"Server");
    }

    /// 樞衡改動 17（G162）：補進表裡的七個名，不建大小寫表時照這裡寫出，用常數插入時也是。
    #[test]
    fn 枢衡改动17_补进标题化表的名照原样写出() {
        let names = [
            (header::ALLOW, "Allow"),
            (header::ALT_SVC, "Alt-Svc"),
            (header::CONTENT_RANGE, "Content-Range"),
            (header::ETAG, "ETag"),
            (header::LAST_MODIFIED, "Last-Modified"),
            (header::LOCATION, "Location"),
            (header::VARY, "Vary"),
        ];
        for (name, titled) in names {
            assert_eq!(titled_header_name_str(&name), Some(titled));
            assert_eq!(name.into_case_header_name().as_slice(), titled.as_bytes());

            let mut resp = ResponseHeader::build_no_case(200, None).unwrap();
            resp.insert_header(titled, "v").unwrap();
            let mut buf: Vec<u8> = vec![];
            resp.header_to_h1_wire(&mut buf);
            assert_eq!(buf, format!("{titled}: v\r\n").into_bytes(), "{titled}");
        }
        // 表外的名照舊寫小寫。
        assert_eq!(titled_header_name_str(&header::EXPIRES), None);
    }
}
