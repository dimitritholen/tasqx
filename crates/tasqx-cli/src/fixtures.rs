//! The captured screens, embedded in the binary.
//!
//! `docs-fixtures/<name>.ansi` is real output from a real run, captured by
//! `scripts/docs-capture.sh` against the demo store on one pinned day
//! (DESIGN.md D149(c)); [`crate::ansi_html::render`] turns one into the HTML
//! `tasqx docs` serves. They are `include_str!`ed rather than read at run time
//! for the reason every other embedded asset here is: `tasqx docs` writes a
//! self-contained page from a single binary, with no data directory to install,
//! nothing to find at run time and nothing to go missing on a user's machine.
//!
//! `include_str!` takes a literal path, so the names are listed ONCE in the
//! macro call below and the table is generated from that list — a second list
//! would be the drift this crate keeps paying for. What the macro cannot check
//! is that the list still agrees with the manifest and with the directory, and
//! that is what `the_manifest_the_directory_and_the_embedded_names_agree`
//! reads all three for, in both directions: a fixture captured but never
//! embedded is invisible to the site, and a name embedded but dropped from the
//! manifest is a screen nothing re-captures and the drift job never checks.

/// Generate [`screen`] and [`names`] from one list of fixture names.
macro_rules! screens {
    ($($name:literal),* $(,)?) => {
        /// One captured screen by name, or `None` when nothing was captured
        /// under that name. The names are the manifest's first column.
        pub fn screen(name: &str) -> Option<&'static str> {
            match name {
                $($name => Some(include_str!(concat!("../docs-fixtures/", $name, ".ansi"))),)*
                _ => None,
            }
        }

        /// Every embedded screen's name, in the manifest's order.
        pub fn names() -> &'static [&'static str] {
            &[$($name),*]
        }
    };
}

screens![
    "init-echo",
    "use-echo",
    "archive-echo",
    "modify-echo",
    "tag-echo",
    "untag-echo",
    "dep-echo",
    "undep-echo",
    "check-echo",
    "cancel-echo",
    "reopen-echo",
    "stop-echo",
    "adjust-echo",
    "config-list",
    "list",
    "list-narrow",
    "agenda",
    "next",
    "show",
    "brief",
    "why",
    "projects",
    "report",
    "chart-burndown",
    "memory-list",
    "memory-search",
    "theme-list",
    "manual",
    "add-echo",
    "start-echo",
    "done-echo",
    "annotate-echo",
    "api-task-list",
    "api-task-get",
    "api-task-done",
    "api-memory-list",
    "api-project-list",
    "api-report-summary",
    "dashboard",
    "pick",
    "memory-browser",
    "api-show-json",
    "api-project-use",
    "api-project-archive",
    "api-task-start",
    "api-task-stop",
    "api-task-adjust-tracked",
    "api-task-modify",
    "api-task-cancel",
    "api-task-reopen",
    "api-tag-add",
    "api-tag-remove",
    "api-annotation-remove",
    "api-check-set",
    "api-check-remove",
    "api-dependency-add",
    "api-dependency-remove",
    "api-graph-query",
    "api-memory-get",
    "api-memory-update",
    "api-memory-remove",
    "api-memory-import",
    "api-tokens-recompute",
    "api-report-outcomes",
    "api-store-import",
    "api-event-list",
    "api-reminder-fire",
    "api-otlp-status",
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("docs-fixtures")
    }

    /// The manifest's first column, comments and the header line skipped.
    fn manifest_names() -> Vec<String> {
        let path = dir().join("manifest.tsv");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        text.lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .map(|l| {
                let row: Vec<&str> = l.split('\t').collect();
                assert_eq!(
                    row.len(),
                    7,
                    "every manifest row is name/kind/cols/rows/args/keys/stdin, \
                     tab-separated — this one has {} field(s):\n{l}",
                    row.len()
                );
                row[0].to_string()
            })
            .collect()
    }

    /// Three lists that must be one: the manifest, the directory, the binary.
    ///
    /// Checked in BOTH directions, because each gap is a different silent
    /// failure. A manifest row with no file is a screen the site asks for and
    /// does not get (`screen()` answers `None`, and the caller unwraps). A file
    /// no row describes is a screen `--check` never re-captures, so it can rot
    /// past every gate. And a name in the macro that is in neither is a compile
    /// error already — which is the half `include_str!` can guard on its own,
    /// and the reason the list lives there.
    #[test]
    fn the_manifest_the_directory_and_the_embedded_names_agree() {
        let manifest: Vec<String> = manifest_names();
        let embedded: Vec<String> = super::names().iter().map(|s| (*s).to_string()).collect();
        assert_eq!(
            manifest, embedded,
            "docs-fixtures/manifest.tsv and the screens![] list in fixtures.rs \
             must name the same screens in the same order"
        );

        let on_disk: BTreeSet<String> = std::fs::read_dir(dir())
            .expect("docs-fixtures is readable")
            .map(|e| e.expect("a readable directory entry").path())
            .filter(|p| p.extension().is_some_and(|e| e == "ansi"))
            .map(|p| {
                p.file_stem()
                    .expect("an .ansi file has a stem")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        let listed: BTreeSet<String> = manifest.iter().cloned().collect();
        assert_eq!(
            listed,
            on_disk,
            "the manifest and docs-fixtures/*.ansi disagree. Missing files: \
             {:?}; files no row describes: {:?}. Re-run scripts/docs-capture.sh.",
            listed.difference(&on_disk).collect::<Vec<_>>(),
            on_disk.difference(&listed).collect::<Vec<_>>(),
        );

        for name in &embedded {
            assert!(
                super::screen(name).is_some_and(|s| !s.is_empty()),
                "{name} embeds nothing — an empty capture is a screen that \
                 printed nothing, which is a defect in the row, not a fixture"
            );
        }
        assert!(
            super::screen("no-such-screen").is_none(),
            "screen() must answer None for a name nothing captured"
        );
    }

    /// The fixtures are captured with colour forced on, and a screen with no
    /// SGR in it is a screen captured through a plain pipe — the defect that
    /// would otherwise reach the site as a grey, weightless table.
    ///
    /// The `api-` rows are exempt, and only those: `tasqx api` writes one JSON
    /// envelope and paints nothing, by design — it is a transport, not a
    /// screen. Exempting them by prefix rather than by a second list keeps the
    /// rule readable: everything tasqx DRAWS is coloured.
    #[test]
    fn every_captured_screen_still_carries_its_colour() {
        let plain: Vec<&&str> = super::names()
            .iter()
            .filter(|n| !n.starts_with("api-"))
            .filter(|n| {
                !super::screen(n)
                    .expect("an embedded screen")
                    .contains('\u{1b}')
            })
            .collect();
        assert!(
            plain.is_empty(),
            "captured with no escape sequence at all, so TASQX_FORCE_COLOR did \
             not reach the binary: {plain:?}"
        );
    }
}
