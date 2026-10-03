use crate::workspace::{Layer, Workspace};
use crate::{Result, pe};

/// Windows refuses to link a DLL exporting more than this many symbols.
const EXPORT_LIMIT: u32 = 65_535;
const EXPORT_WARNING: u32 = 50_000;

/// Layers each layer may depend on, inside the repository (specification, section 2).
fn allowed(from: Layer) -> &'static [Layer] {
    match from {
        Layer::Feature | Layer::Kernel => &[Layer::Api, Layer::Lib],
        Layer::Api | Layer::Lib => &[Layer::Lib],
        Layer::App => &[Layer::Api, Layer::Kernel],
        Layer::Xtask | Layer::Unknown => &[],
    }
}

pub fn run() -> Result {
    let ws = Workspace::load()?;
    let mut problems = Vec::new();

    for package in &ws.packages {
        for dependency in &package.dependencies {
            let Some(path) = &dependency.path else {
                continue;
            };
            let target = ws.layer_of(path);
            if !allowed(package.layer).contains(&target) {
                problems.push(format!(
                    "{} ({:?}) depends on {} ({:?}), which is not allowed",
                    package.name, package.layer, dependency.name, target
                ));
            }
        }
        if package.layer == Layer::Feature {
            let folder = package
                .dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if package.name != format!("uniwow-feature-{folder}") {
                problems.push(format!(
                    "features/{folder}: the package must be named uniwow-feature-{folder}"
                ));
            }
            if package.feature_id().as_deref() != Some(folder.as_str()) {
                problems.push(format!(
                    "features/{folder}: [package.metadata.uniwow] id must be \"{folder}\""
                ));
            }
            if !package.crate_types.iter().any(|c| c == "cdylib") {
                problems.push(format!("features/{folder}: crate-type must be [\"cdylib\"]"));
            }
        }
    }

    for profile in ["debug", "release"] {
        let dll = ws.target_dir.join(profile).join(crate::RUNTIME_DLL);
        if let Ok(count) = pe::exported_names(&dll) {
            let percent = count as f32 * 100.0 / EXPORT_LIMIT as f32;
            println!("runtime ({profile}): {count} exported symbols, {percent:.0}% of the Windows limit");
            if count >= EXPORT_WARNING {
                problems.push(format!(
                    "the {profile} runtime exports {count} symbols, close to the limit of {EXPORT_LIMIT}: split it"
                ));
            }
        }
    }

    if problems.is_empty() {
        println!("check passed: {} packages", ws.packages.len());
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}
