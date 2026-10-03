use crate::workspace::{Dependency, Layer, Workspace};
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
    let mut problems = problems(&ws);

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

/// Every breach of the dependency and naming rules in the workspace.
pub fn problems(ws: &Workspace) -> Vec<String> {
    let mut problems = Vec::new();
    // Each library is compiled once, into the runtime: otherwise every feature using it would carry
    // its own copy, with its own globals, and its dependencies could rebuild the runtime.
    let api = ws.packages.iter().find(|p| p.layer == Layer::Api);
    for library in ws.packages.iter().filter(|p| p.layer == Layer::Lib) {
        let in_runtime = api.is_some_and(|api| {
            api.dependencies
                .iter()
                .any(|d| d.kind == "normal" && d.path.is_some() && d.name == library.name)
        });
        if !in_runtime {
            problems.push(format!(
                "{} is not a normal dependency of uniwow-api: every crate of libs/ must be part of the runtime",
                library.name
            ));
        }
    }
    for package in &ws.packages {
        for dependency in &package.dependencies {
            if package.layer == Layer::Feature {
                // Strict: any other crate, even one already inside the runtime, may enable an option
                // that makes Cargo rebuild the runtime, which changes its fingerprint.
                let inside = dependency.path.as_ref().map(|p| ws.layer_of(p));
                if !matches!(inside, Some(Layer::Api | Layer::Lib)) {
                    problems.push(format!(
                        "{} depends on {} ({} dependency, {}): a feature may only depend on uniwow-api and libs/*",
                        package.name,
                        dependency.name,
                        dependency.kind,
                        origin(dependency)
                    ));
                }
                continue;
            }
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
    problems
}

fn origin(dependency: &Dependency) -> String {
    match (&dependency.path, &dependency.source) {
        (Some(path), _) => format!("path {}", path.display()),
        (None, Some(source)) if source.starts_with("registry+") => "crates.io".to_owned(),
        (None, Some(source)) if source.starts_with("git+") => format!("git {}", &source[4..]),
        (None, Some(source)) => source.clone(),
        (None, None) => "unknown source".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::problems;
    use crate::workspace::Workspace;

    const ROOT: &str = "/ws";

    fn path_dependency(name: &str, path: &str, kind: Option<&str>) -> Value {
        json!({ "name": name, "kind": kind, "path": format!("{ROOT}/{path}"), "source": null })
    }

    fn registry_dependency(name: &str, kind: Option<&str>) -> Value {
        json!({ "name": name, "kind": kind, "source": "registry+https://github.com/rust-lang/crates.io-index" })
    }

    fn package(name: &str, dir: &str, dependencies: Vec<Value>, crate_type: &str, id: Option<&str>) -> Value {
        json!({
            "name": name,
            "version": "0.1.0",
            "description": "",
            "manifest_path": format!("{ROOT}/{dir}/Cargo.toml"),
            "dependencies": dependencies,
            "targets": [{ "kind": [crate_type], "crate_types": [crate_type] }],
            "metadata": id.map(|id| json!({ "uniwow": { "id": id } })),
        })
    }

    fn feature(id: &str, dependencies: Vec<Value>) -> Value {
        let mut all = vec![path_dependency("uniwow-api", "core/api", None)];
        all.extend(dependencies);
        package(
            &format!("uniwow-feature-{id}"),
            &format!("features/{id}"),
            all,
            "cdylib",
            Some(id),
        )
    }

    /// A valid workspace plus `extra` packages.
    fn workspace(extra: Vec<Value>) -> Workspace {
        let mut packages = vec![
            package(
                "uniwow-api",
                "core/api",
                vec![
                    registry_dependency("eframe", None),
                    path_dependency("uniwow-gpu", "libs/gpu", None),
                ],
                "dylib",
                None,
            ),
            package(
                "uniwow-kernel",
                "core/kernel",
                vec![
                    path_dependency("uniwow-api", "core/api", None),
                    registry_dependency("libloading", None),
                ],
                "lib",
                None,
            ),
            package("uniwow-gpu", "libs/gpu", vec![], "lib", None),
            feature("viewport", vec![path_dependency("uniwow-gpu", "libs/gpu", None)]),
        ];
        packages.extend(extra);
        Workspace::from_metadata(&json!({
            "workspace_root": ROOT,
            "target_directory": format!("{ROOT}/target"),
            "packages": packages,
        }))
    }

    #[test]
    fn a_valid_workspace_passes() {
        assert_eq!(problems(&workspace(vec![])), Vec::<String>::new());
    }

    #[test]
    fn a_feature_may_not_use_a_crates_io_crate() {
        let found = problems(&workspace(vec![feature(
            "cube",
            vec![registry_dependency("rand", None)],
        )]));
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("uniwow-feature-cube depends on rand"), "{found:?}");
        assert!(found[0].contains("crates.io"), "{found:?}");
    }

    #[test]
    fn dev_and_build_dependencies_are_checked_too() {
        let found = problems(&workspace(vec![feature(
            "cube",
            vec![
                registry_dependency("rand", Some("dev")),
                registry_dependency("cc", Some("build")),
            ],
        )]));
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains("rand (dev dependency"), "{found:?}");
        assert!(found[1].contains("cc (build dependency"), "{found:?}");
    }

    #[test]
    fn a_feature_may_not_depend_on_another_feature() {
        let found = problems(&workspace(vec![feature(
            "cube",
            vec![path_dependency("uniwow-feature-viewport", "features/viewport", None)],
        )]));
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("depends on uniwow-feature-viewport"), "{found:?}");
    }

    #[test]
    fn a_feature_may_not_use_a_path_outside_the_layers() {
        let found = problems(&workspace(vec![feature(
            "cube",
            vec![path_dependency("helper", "tools/helper", None)],
        )]));
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("depends on helper"), "{found:?}");
    }

    #[test]
    fn the_kernel_may_not_depend_on_a_feature() {
        let found = problems(&workspace(vec![package(
            "uniwow-app",
            "app",
            vec![path_dependency("uniwow-feature-viewport", "features/viewport", None)],
            "bin",
            None,
        )]));
        assert_eq!(found.len(), 1);
        assert!(
            found[0].contains("uniwow-app (App) depends on uniwow-feature-viewport"),
            "{found:?}"
        );
    }

    #[test]
    fn a_library_outside_the_runtime_is_refused_even_unused() {
        let found = problems(&workspace(vec![package(
            "uniwow-extra",
            "libs/extra",
            vec![],
            "lib",
            None,
        )]));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].starts_with("uniwow-extra is not a normal dependency of uniwow-api"),
            "{found:?}"
        );
    }

    #[test]
    fn a_library_outside_the_runtime_is_refused_when_a_feature_uses_it() {
        let found = problems(&workspace(vec![
            package("uniwow-extra", "libs/extra", vec![], "lib", None),
            feature("cube", vec![path_dependency("uniwow-extra", "libs/extra", None)]),
        ]));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].starts_with("uniwow-extra is not a normal dependency of uniwow-api"),
            "{found:?}"
        );
    }

    #[test]
    fn feature_names_ids_and_crate_types_are_checked() {
        let wrong = package(
            "cube",
            "features/cube",
            vec![path_dependency("uniwow-api", "core/api", None)],
            "lib",
            Some("other"),
        );
        let found = problems(&workspace(vec![wrong]));
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found.iter().any(|p| p.contains("must be named uniwow-feature-cube")));
        assert!(found.iter().any(|p| p.contains("id must be \"cube\"")));
        assert!(found.iter().any(|p| p.contains("crate-type must be")));
    }
}
