//! Shared build-time metadata for the three Windows driver images.
//! No runtime dependency, Python requirement, or Windows library imports.

use std::{collections::BTreeMap, error::Error, path::Path};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub fn read_values(path: &Path) -> Result<BTreeMap<String, String>> {
    println!("cargo:rerun-if-changed={}", path.display());
    let mut values = BTreeMap::new();
    for line in std::fs::read_to_string(path)?.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or("invalid metadata assignment")?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            || value.is_empty()
            || !value
                .bytes()
                .all(|b| (32..=126).contains(&b) && b != b'"' && b != b'\\')
            || values.insert(key.to_string(), value.to_string()).is_some()
        {
            return Err(format!("invalid or duplicate metadata key: {key}").into());
        }
    }
    Ok(values)
}

pub fn parse_version(raw: &str) -> Result<[u16; 4]> {
    if !raw.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return Err("driver version must contain only decimal components".into());
    }
    let parts = raw
        .split('.')
        .map(str::parse::<u16>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    parts
        .try_into()
        .map_err(|_| "driver version must contain four 16-bit components".into())
}

pub fn render(root: &Path, component: &str) -> Result<String> {
    let values = read_values(&root.join("metadata/helios.env"))?;
    let get = |key: &str| {
        values
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| format!("missing {key}"))
    };
    let product = get("HELIOS_PRODUCT")?;
    // EDID has 13 bytes: at most 12 printable name bytes plus a newline.
    if product.len() > 12 {
        return Err("HELIOS_PRODUCT exceeds the EDID monitor-name limit (12 bytes)".into());
    }
    let publisher = get("HELIOS_PUBLISHER")?;
    let (role, filename, file_type, subtype) = match component {
        "kmd_render" => ("HELIOS_KMD_ROLE", "helios_kmd_render.sys", 3, 4),
        "umd" => ("HELIOS_UMD_ROLE", "helios_umd.dll", 2, 0),
        "umd12" => ("HELIOS_UMD12_ROLE", "helios_umd12.dll", 2, 0),
        _ => return Err(format!("unknown driver component: {component}").into()),
    };
    let description = format!("{product} {}", get(role)?);
    let version = read_values(&root.join("kmd_render/driver-version.env"))?;
    let raw = version
        .get("HELIOS_KMD_VERSION")
        .ok_or("missing HELIOS_KMD_VERSION")?;
    let v = parse_version(raw)?;
    let dotted = format!("{}.{}.{}.{}", v[0], v[1], v[2], v[3]);
    let comma = dotted.replace('.', ",");
    let internal = filename
        .rsplit_once('.')
        .ok_or("missing filename extension")?
        .0;
    let mut resource = include_str!("version.rc.in").to_string();
    for (key, value) in [
        ("@COMMA@", comma.as_str()),
        ("@FILE_TYPE@", if file_type == 3 { "3" } else { "2" }),
        ("@SUBTYPE@", if subtype == 4 { "4" } else { "0" }),
        ("@PUBLISHER@", publisher),
        ("@DESCRIPTION@", description.as_str()),
        ("@DOTTED@", dotted.as_str()),
        ("@INTERNAL@", internal),
        ("@FILENAME@", filename),
        ("@PRODUCT@", product),
    ] {
        resource = resource.replace(key, value);
    }
    Ok(resource)
}

pub fn compile(component: &str) -> Result<()> {
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let root = manifest.parent().ok_or("missing repository root")?;
    println!(
        "cargo:rerun-if-changed={}",
        root.join("metadata/windows_resource.rs").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        root.join("metadata/version.rc.in").display()
    );
    let resource = render(root, component)?;
    let values = read_values(&root.join("metadata/helios.env"))?;
    let product = &values["HELIOS_PRODUCT"];
    if component == "kmd_render" {
        // The INX must remain a usable source file for WDK packaging. Fail a
        // hand-run cargo build as well as CI if its synchronized strings drift.
        let inx = root.join("kmd_render/helios_kmd_render.inx");
        println!("cargo:rerun-if-changed={}", inx.display());
        let text = std::fs::read_to_string(inx)?;
        for expected in [
            format!("ProviderName = \"{}\"", values["HELIOS_PUBLISHER"]),
            format!("DeviceDesc   = \"{product}\""),
            format!("DiskName     = \"{product} Driver Package\""),
            "HKR,, HardwareInformation.AdapterString, %REG_SZ%, %DeviceDesc%".to_string(),
        ] {
            if !text.lines().any(|line| line == expected) {
                return Err(format!(
                    "stale INX metadata: run python3 tools/sync-metadata.py ({expected})"
                )
                .into());
            }
        }
        let publisher = &values["HELIOS_PUBLISHER"];
        if publisher.len() > 12 {
            return Err("publisher exceeds the EDID ASCII-text limit (12 bytes)".into());
        }
        let year: u16 = values
            .get("HELIOS_MONITOR_MODEL_YEAR")
            .ok_or("missing HELIOS_MONITOR_MODEL_YEAR")?
            .parse()?;
        if !(1990..=2245).contains(&year) {
            return Err("EDID model year must be between 1990 and 2245".into());
        }
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
        std::fs::write(out.join("monitor_metadata.rs"), format!(
            "pub const NAME: &str = {product:?};\npub const PUBLISHER: &str = {publisher:?};\npub const MODEL_YEAR: u16 = {year};\n"
        ))?;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    let rc_path = out.join(format!("{component}_version.rc"));
    let res_path = out.join(format!("{component}_version.res"));
    std::fs::write(&rc_path, resource)?;
    let rc = find_windows_sdk_tool();
    let status = std::process::Command::new(&rc)
        .arg("/nologo")
        .arg(format!("/fo{}", res_path.display()))
        .arg(&rc_path)
        .status()?;
    if !status.success() {
        return Err(format!("{} failed with {status}", rc.display()).into());
    }
    println!("cargo:rustc-link-arg={}", res_path.display());
    Ok(())
}

fn find_windows_sdk_tool() -> std::path::PathBuf {
    for key in ["WindowsSdkDir", "WindowsSDKVersion"] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    if let (Ok(dir), Ok(version)) = (
        std::env::var("WindowsSdkDir"),
        std::env::var("WindowsSDKVersion"),
    ) {
        let candidate = Path::new(&dir)
            .join("bin")
            .join(version.trim_end_matches('\\'))
            .join("x64/rc.exe");
        if candidate.exists() {
            return candidate;
        }
    }
    let candidate =
        Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\rc.exe");
    if candidate.exists() {
        return candidate.into();
    }
    "rc.exe".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_versions_fit_windows_words() {
        assert_eq!(parse_version("0.1.65535.0").unwrap(), [0, 1, 65535, 0]);
        for invalid in [
            "1.2.3",
            "1.2.3.4.5",
            "1.2.65536.0",
            "-1.2.3.4",
            "1.2..4",
            "+1.2.3.4",
        ] {
            assert!(parse_version(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn malformed_metadata_fails_instead_of_defaulting() {
        let path =
            std::env::temp_dir().join(format!("helios-metadata-test-{}.env", std::process::id()));
        for invalid in [
            "KEY=one\nKEY=two\n",
            "KEY=\n",
            "KEY=bad\"quote\n",
            "KEY=bad\\escape\n",
            "not an assignment\n",
        ] {
            std::fs::write(&path, invalid).unwrap();
            assert!(read_values(&path).is_err(), "accepted {invalid}");
        }
        std::fs::write(&path, "# comment\nHELIOS_UMD12_ROLE=Direct3D 12\n").unwrap();
        assert_eq!(
            read_values(&path).unwrap()["HELIOS_UMD12_ROLE"],
            "Direct3D 12"
        );
        std::fs::remove_file(path).unwrap();
    }
}
