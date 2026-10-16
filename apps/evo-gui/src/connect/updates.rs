//! Official TI-84 Evo OS download metadata. Blocking: run these on a worker thread.
//! All external IO goes through the platform seam; downloading does not install an OS.

use crate::platform::Platform;
use serde::Deserialize;

const BASE: &str = "https://connectevo.ti.com/ticevo/en/assets/os/";
const MANIFEST: &str = "https://connectevo.ti.com/ticevo/en/assets/os/version";
const MANIFEST_LIMIT: usize = 64 * 1024;
const BUNDLE_LIMIT: usize = 32 * 1024 * 1024;

fn validate_version(version: &str) -> Result<(), String> {
    let parts: Vec<_> = version.split('.').collect();
    if version.len() > 40
        || parts.len() != 4
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.bytes().all(|c| c.is_ascii_digit())
                || part.parse::<u32>().is_err()
        })
    {
        return Err("The OS version must contain four numeric components".into());
    }
    Ok(())
}

pub fn check_latest(platform: &dyn Platform) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Manifest {
        #[serde(rename = "ti84Bundle")]
        version: String,
    }
    let bytes = platform.http_get_bytes(MANIFEST, MANIFEST_LIMIT)?;
    if bytes.len() > MANIFEST_LIMIT {
        return Err("TI OS metadata exceeds 64 KiB".into());
    }
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|e| format!("Invalid TI OS metadata: {e}"))?;
    validate_version(&manifest.version)?;
    Ok(manifest.version)
}

/// Return the official filename and bytes. The backend validates the actual bundle
/// before connecting to the calculator or transferring any data.
pub fn download(platform: &dyn Platform, version: &str) -> Result<(String, Vec<u8>), String> {
    validate_version(version)?;
    let filename = format!("TI84Evo_Bundle_{version}.84b2");
    let bytes = platform.http_get_bytes(&format!("{BASE}{filename}"), BUNDLE_LIMIT)?;
    if bytes.len() > BUNDLE_LIMIT {
        return Err("TI OS bundle exceeds 32 MiB".into());
    }
    if !bytes.starts_with(b"PK\x03\x04") {
        return Err("The OS download is not a ZIP bundle".into());
    }
    Ok((filename, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::FakePlatform;

    #[test]
    fn selects_correct_product_and_downloads_using_platform() {
        let platform = FakePlatform::default();
        platform.set_http_bytes(
            MANIFEST,
            Ok(br#"{"ti84Bundle":"7.1.0.4421","ti84sBundle":"7.1.0.4404"}"#.to_vec()),
        );
        let version = check_latest(&platform).unwrap();
        assert_eq!(version, "7.1.0.4421");
        let url = format!("{BASE}TI84Evo_Bundle_{version}.84b2");
        platform.set_http_bytes(&url, Ok(b"PK\x03\x04synthetic ZIP header".to_vec()));
        let (name, bytes) = download(&platform, &version).unwrap();
        assert_eq!(name, "TI84Evo_Bundle_7.1.0.4421.84b2");
        assert!(bytes.starts_with(b"PK\x03\x04"));
    }

    #[test]
    fn rejects_invalid_metadata_versions_and_nonzip_downloads() {
        let platform = FakePlatform::default();
        for bad in [
            "",
            "7.1.0",
            "7.1.0.1/../../other",
            "7.1.0.1?x",
            "7.1.0.-1",
            "7.1.0.99999999999",
            "７.1.0.1",
        ] {
            assert!(
                download(&platform, bad)
                    .unwrap_err()
                    .contains("four numeric"),
                "{bad}"
            );
        }
        for bad in [
            br#"{"ti84sBundle":"7.1.0.1"}"#.as_slice(),
            br#"{"ti84Bundle":7}"#,
            br#"{"ti84Bundle":"7.1.0.1/evil"}"#,
            b"not JSON",
        ] {
            platform.set_http_bytes(MANIFEST, Ok(bad.to_vec()));
            assert!(check_latest(&platform).is_err());
        }
        platform.set_http_bytes(
            &format!("{BASE}TI84Evo_Bundle_7.1.0.1.84b2"),
            Ok(b"<html>error</html>".to_vec()),
        );
        assert!(download(&platform, "7.1.0.1").unwrap_err().contains("ZIP"));
    }

    #[test]
    fn propagates_http_failures_and_enforces_body_limits() {
        let platform = FakePlatform::default();
        platform.set_http_bytes(MANIFEST, Err("HTTP 503".into()));
        assert_eq!(check_latest(&platform).unwrap_err(), "HTTP 503");
        platform.set_http_bytes(MANIFEST, Ok(vec![b' '; MANIFEST_LIMIT + 1]));
        assert!(check_latest(&platform).is_err());
        platform.set_http_bytes(
            &format!("{BASE}TI84Evo_Bundle_7.1.0.1.84b2"),
            Ok(vec![0; BUNDLE_LIMIT + 1]),
        );
        assert!(download(&platform, "7.1.0.1").is_err());
    }
}
