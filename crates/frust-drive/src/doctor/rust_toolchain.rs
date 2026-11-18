use super::{DoctorCtx, Status, Validation, Validator};

/// Minimum supported `rustc` version (matches the workspace `rust-version`).
const MIN_RUST_VERSION: (u64, u64, u64) = (1, 89, 0);

pub struct RustToolchainValidator;

impl Validator for RustToolchainValidator {
    fn name(&self) -> &str {
        "Rust toolchain"
    }

    fn validate(&self, ctx: &DoctorCtx) -> Validation {
        let mut messages = Vec::new();
        let mut status = Status::Pass;

        match ctx.runner.run("rustc", &["--version"]) {
            Ok(out) if out.success => match parse_version(&out.stdout) {
                Some(version) if version >= MIN_RUST_VERSION => {
                    messages.push(format!("rustc {}.{}.{}", version.0, version.1, version.2));
                }
                Some(version) => {
                    status = Status::Fail;
                    messages.push(format!(
                        "rustc {}.{}.{} found, but Frust requires >= {}.{}.{}. Run `rustup update`.",
                        version.0, version.1, version.2, MIN_RUST_VERSION.0, MIN_RUST_VERSION.1, MIN_RUST_VERSION.2
                    ));
                }
                None => {
                    status = Status::Partial;
                    messages.push(format!(
                        "could not parse rustc version from: {}",
                        out.stdout.trim()
                    ));
                }
            },
            _ => {
                status = Status::Fail;
                messages.push("rustc not found. Install via https://rustup.rs".to_string());
            }
        }

        match ctx.runner.run("cargo", &["--version"]) {
            Ok(out) if out.success => messages.push("cargo present".to_string()),
            _ => {
                status = Status::Fail;
                messages.push("cargo not found. Install via https://rustup.rs".to_string());
            }
        }

        Validation { status, messages }
    }
}

/// Parses `"rustc 1.91.1 (ed61e7d7e 2025-11-07)"` into `(1, 91, 1)`.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let version = text.split_whitespace().nth(1)?;
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::FakeEnv;
    use crate::process::{FakeProcessRunner, Output};

    fn ok(stdout: &str) -> Output {
        Output {
            success: true,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    #[test]
    fn passes_when_version_meets_minimum() {
        let runner = FakeProcessRunner::new()
            .with(
                "rustc --version",
                ok("rustc 1.91.1 (ed61e7d7e 2025-11-07)\n"),
            )
            .with("cargo --version", ok("cargo 1.91.1\n"));
        let env = FakeEnv::new();
        let ctx = DoctorCtx {
            runner: &runner,
            env: &env,
            is_macos: true,
        };
        let result = RustToolchainValidator.validate(&ctx);
        assert_eq!(result.status, Status::Pass);
    }

    #[test]
    fn fails_when_version_below_minimum() {
        let runner = FakeProcessRunner::new()
            .with("rustc --version", ok("rustc 1.70.0 (abc 2023-01-01)\n"))
            .with("cargo --version", ok("cargo 1.70.0\n"));
        let env = FakeEnv::new();
        let ctx = DoctorCtx {
            runner: &runner,
            env: &env,
            is_macos: true,
        };
        let result = RustToolchainValidator.validate(&ctx);
        assert_eq!(result.status, Status::Fail);
    }

    #[test]
    fn fails_when_rustc_missing() {
        let runner = FakeProcessRunner::new()
            .missing("rustc --version")
            .with("cargo --version", ok("cargo 1.91.1"));
        let env = FakeEnv::new();
        let ctx = DoctorCtx {
            runner: &runner,
            env: &env,
            is_macos: true,
        };
        let result = RustToolchainValidator.validate(&ctx);
        assert_eq!(result.status, Status::Fail);
        assert!(result.messages.iter().any(|m| m.contains("rustup.rs")));
    }

    fn status_for(rustc_version_line: &str) -> Status {
        let runner = FakeProcessRunner::new()
            .with("rustc --version", ok(rustc_version_line))
            .with("cargo --version", ok("cargo 1.91.1\n"));
        let env = FakeEnv::new();
        let ctx = DoctorCtx {
            runner: &runner,
            env: &env,
            is_macos: true,
        };
        RustToolchainValidator.validate(&ctx).status
    }

    #[test]
    fn fails_on_rustc_1_88() {
        assert_eq!(
            status_for("rustc 1.88.0 (6b00bc388 2025-06-23)\n"),
            Status::Fail
        );
    }

    #[test]
    fn passes_on_rustc_1_89_and_newer() {
        assert_eq!(
            status_for("rustc 1.89.0 (29483883e 2025-08-04)\n"),
            Status::Pass
        );
        assert_eq!(
            status_for("rustc 1.98.1 (abcdef012 2026-09-01)\n"),
            Status::Pass
        );
    }

    #[test]
    fn minimum_matches_workspace_rust_version() {
        let manifest =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml"))
                .expect("read root Cargo.toml");
        let mut in_section = false;
        let mut declared = None;
        for line in manifest.lines().map(str::trim) {
            if line.starts_with('[') {
                in_section = line == "[workspace.package]";
            } else if in_section && line.starts_with("rust-version") {
                declared = line.split('"').nth(1).map(str::to_string);
                break;
            }
        }
        let declared = declared.expect("[workspace.package] rust-version");
        assert_eq!(
            declared,
            format!("{}.{}", MIN_RUST_VERSION.0, MIN_RUST_VERSION.1)
        );
    }

    #[test]
    fn parses_version_string() {
        assert_eq!(
            parse_version("rustc 1.88.0 (deadbeef 2025-01-01)"),
            Some((1, 88, 0))
        );
        assert_eq!(parse_version("garbage"), None);
    }
}
