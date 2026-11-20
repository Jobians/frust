//! The project-aware half of `frust doctor` for built-in plugins: which ones
//! an app depends on but has not finished wiring (a missing Gradle module
//! include, a stock activity theme).
//!
//! `frust run`, `frust build` and `frust plugin sync` leave built-in plugins
//! alone, so these two setup mistakes would otherwise show only at run time
//! (a blank native slot, frust-drawn fallback bars). The judgement is the
//! plugin system's own ([`pending_contributions`]); nothing here parses a
//! Gradle or XML file. Pure and print-free: the CLI renders [`PluginFinding::lines`].

use std::path::Path;

use crate::plugin::{
    Contribution, PluginCatalog, PluginStatus, pending_contributions, plugin_statuses,
};

/// The limitation entry that explains a stock activity theme.
const THEME_LIMITATION: &str = "native-chrome-android-material3-theme-required";

/// One line of the `Plugins` block's input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginFinding {
    /// A built-in plugin the app depends on that still has pending edits.
    Pending {
        /// The plugin's id.
        plugin_id: String,
        /// [`Contribution::describe`] of each pending contribution; never empty.
        pending: Vec<String>,
        /// The limitation entry explaining a pending contribution, if any.
        limitation: Option<&'static str>,
        /// What applies the edits: the add command, preceded by the manual
        /// edits when the add would refuse a pending contribution
        /// ([`crate::plugin::PendingContribution::manual_edits`]).
        fix: String,
    },
    /// A check that could not be made; never a failure.
    Note(String),
}

impl PluginFinding {
    /// The finding as printed lines: a heading row, then indented detail rows.
    pub fn lines(&self) -> Vec<String> {
        match self {
            Self::Pending {
                plugin_id,
                pending,
                limitation,
                fix,
            } => {
                let mut lines = vec![format!("[!] {plugin_id}")];
                for item in pending {
                    lines.push(format!("    pending: {item}"));
                }
                if let Some(id) = limitation {
                    lines.push(format!("    see limitation: {id}"));
                }
                lines.push(format!("    fix: {fix}"));
                lines
            }
            Self::Note(text) => vec![format!("    note: {text}")],
        }
    }
}

/// The built-in plugins the app at `project_root` depends on directly and has
/// not finished wiring, in catalog order. Empty when everything is in place.
/// Any error reading the project becomes a [`PluginFinding::Note`]. Plugins
/// described by a dependency's own manifest are not reported: `frust plugin
/// sync` and the run/build stop own them.
pub fn plugin_findings(project_root: &Path) -> Vec<PluginFinding> {
    let catalog = PluginCatalog::built_in();
    let statuses = match plugin_statuses(project_root, &catalog) {
        Ok(statuses) => statuses,
        Err(err) => {
            return vec![PluginFinding::Note(format!(
                "could not check the project's plugins for pending changes: {err}"
            ))];
        }
    };
    let mut findings = Vec::new();
    for (entry, status) in statuses {
        let id = entry.spec.id.to_string();
        match status {
            PluginStatus::NotInstalled | PluginStatus::Added => {}
            PluginStatus::CheckFailed(err) => findings.push(PluginFinding::Note(format!(
                "could not check plugin `{id}` for pending changes: {err}"
            ))),
            PluginStatus::Pending(_) => match pending_contributions(project_root, entry, &[]) {
                Ok(pending) if pending.is_empty() => {}
                Ok(pending) => {
                    let limitation = pending
                        .iter()
                        .any(|p| matches!(p.contribution, Contribution::AndroidThemeParent { .. }))
                        .then_some(THEME_LIMITATION);
                    let manual: Vec<String> =
                        pending.iter().filter_map(|p| p.manual_edits()).collect();
                    let fix = if manual.is_empty() {
                        format!("frust plugin add {id}")
                    } else {
                        format!("{}, then re-run frust plugin add {id}", manual.join("; "))
                    };
                    findings.push(PluginFinding::Pending {
                        fix,
                        pending: pending.iter().map(|p| p.describe()).collect(),
                        plugin_id: id,
                        limitation,
                    });
                }
                Err(err) => findings.push(PluginFinding::Note(format!(
                    "could not check plugin `{id}` for pending changes: {err}"
                ))),
            },
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packages::StubLocator;
    use crate::plugin::add_plugin_with;
    use crate::scaffold::{self, TemplateContext};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_root(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "frust-drive-doctor-plugins-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        crate::host_path::canonicalize_simplified(&dir).unwrap()
    }

    /// A scaffolded app with native-widgets added: dependency, Gradle
    /// module include and Material 3 theme parent all in place.
    fn app_with_native_widgets(tag: &str) -> PathBuf {
        let root = temp_root(tag);
        let ctx = TemplateContext {
            project_name: "acme_app".into(),
            title_case_name: "Acme App".into(),
            org: "com.example".into(),
            description: "A test app.".into(),
            frust_version: "0.1.0".into(),
            frust: scaffold::FrustDependency::Registry {
                version: "0.1.0".into(),
            },
            deeplink_scheme: None,
            deeplink_host: None,
        };
        scaffold::generate(&root, &ctx, None, false, None).unwrap();
        let locator = StubLocator::new().with("frust-native-widgets", root.join("nw"));
        add_plugin_with(&locator, &root, "native-widgets", &[]).unwrap();
        root
    }

    #[test]
    fn a_fully_wired_app_has_no_findings() {
        let root = app_with_native_widgets("wired");
        assert_eq!(plugin_findings(&root), vec![]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_app_without_built_in_plugins_has_no_findings() {
        let root = temp_root("none");
        let ctx = TemplateContext {
            project_name: "acme_app".into(),
            title_case_name: "Acme App".into(),
            org: "com.example".into(),
            description: "A test app.".into(),
            frust_version: "0.1.0".into(),
            frust: scaffold::FrustDependency::Registry {
                version: "0.1.0".into(),
            },
            deeplink_scheme: None,
            deeplink_host: None,
        };
        scaffold::generate(&root, &ctx, None, false, None).unwrap();
        assert_eq!(plugin_findings(&root), vec![]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_removed_gradle_include_names_the_plugin_and_the_fix() {
        let root = app_with_native_widgets("include");
        let settings = root.join("android/settings.gradle.kts");
        let kept: String = fs::read_to_string(&settings)
            .unwrap()
            .lines()
            .filter(|l| !l.contains("frust-native-widgets"))
            .map(|l| format!("{l}\n"))
            .collect();
        fs::write(&settings, kept).unwrap();
        let findings = plugin_findings(&root);
        let [
            PluginFinding::Pending {
                plugin_id,
                pending,
                limitation,
                fix,
            },
        ] = findings.as_slice()
        else {
            panic!("{findings:?}");
        };
        assert_eq!(plugin_id, "native-widgets");
        assert_eq!(fix, "frust plugin add native-widgets");
        assert!(pending.iter().any(|p| p.contains("frust-native-widgets")));
        assert_eq!(*limitation, None);
        let text = findings[0].lines().join("\n");
        assert!(text.contains("frust plugin add native-widgets"), "{text}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_reverted_theme_parent_names_the_limitation() {
        let root = app_with_native_widgets("theme");
        let themes = root.join("android/app/src/main/res/values/themes.xml");
        let text = fs::read_to_string(&themes).unwrap();
        assert!(text.contains("Theme.Material3.DayNight.NoActionBar"));
        fs::write(
            &themes,
            text.replace(
                "Theme.Material3.DayNight.NoActionBar",
                "android:Theme.Material.NoActionBar",
            ),
        )
        .unwrap();
        let findings = plugin_findings(&root);
        let [
            PluginFinding::Pending {
                limitation, fix, ..
            },
        ] = findings.as_slice()
        else {
            panic!("{findings:?}");
        };
        assert_eq!(*limitation, Some(THEME_LIMITATION));
        // A platform parent is rewritable: the add itself is the fix.
        assert_eq!(fix, "frust plugin add native-widgets");
        let lines = findings[0].lines().join("\n");
        assert!(lines.contains(THEME_LIMITATION), "{lines}");
        assert!(lines.contains("Android theme `FrustAppTheme`"), "{lines}");
        let _ = fs::remove_dir_all(&root);
    }

    /// An activity theme that is neither `FrustAppTheme` nor a Material one
    /// makes `frust plugin add` refuse, so the fix names the two manual edits
    /// before the add.
    #[test]
    fn an_unthemed_activity_names_the_manual_edits_then_the_add() {
        let root = app_with_native_widgets("unthemed");
        let manifest = root.join("android/app/src/main/AndroidManifest.xml");
        let text = fs::read_to_string(&manifest).unwrap();
        assert!(text.contains("@style/FrustAppTheme"));
        fs::write(
            &manifest,
            text.replace(
                "@style/FrustAppTheme",
                "@android:style/Theme.Material.NoActionBar",
            ),
        )
        .unwrap();
        let findings = plugin_findings(&root);
        let [
            PluginFinding::Pending {
                limitation, fix, ..
            },
        ] = findings.as_slice()
        else {
            panic!("{findings:?}");
        };
        assert_eq!(*limitation, Some(THEME_LIMITATION));
        assert_ne!(fix, "frust plugin add native-widgets");
        assert!(
            fix.contains(
                "`<style name=\"FrustAppTheme\" parent=\"Theme.Material3.DayNight.NoActionBar\" />`"
            ),
            "{fix}"
        );
        assert!(
            fix.contains("android/app/src/main/res/values/themes.xml"),
            "{fix}"
        );
        assert!(
            fix.contains("android:theme=\"@style/FrustAppTheme\" on both the <application> and the <activity>"),
            "{fix}"
        );
        assert!(
            fix.contains("android/app/src/main/AndroidManifest.xml"),
            "{fix}"
        );
        assert!(
            fix.ends_with("then re-run frust plugin add native-widgets"),
            "{fix}"
        );
        let lines = findings[0].lines().join("\n");
        assert!(lines.contains(&format!("fix: {fix}")), "{lines}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unreadable_project_is_one_note() {
        let root = temp_root("unreadable");
        let findings = plugin_findings(&root);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(matches!(&findings[0], PluginFinding::Note(t) if t.contains("could not check")));
        let _ = fs::remove_dir_all(&root);
    }
}
