//! Settings as a surface: `tasqx config`/`theme` and the interactive settings
//! screen's driver, plus the typed readers (`config_*`) the daemon and the
//! dashboard consult. The resolution rules themselves live in `config` (the
//! registry) — this module is the CLI's client of them, split out of lib.rs
//! along the seam the registry-bypass fixes had just cleaned.

use super::*;

/// Resolve the active theme (flag > $TASQX_THEME > config > default) and detect
/// terminal capability, producing the render context every command shares.
/// Warn, but never refuse, when a theme name cannot be honoured.
///
/// `theme set` and `config set` REJECT an unknown name because they persist it,
/// and a persisted name that silently does nothing is a lie the user carries
/// forever. `--theme` and `$TASQX_THEME` apply to one invocation, so the same
/// treatment would be wrong in the other direction: `tasqx --theme typo add
/// "urgent thing"` must still capture the task. Refusing to record work because
/// a colour scheme was misspelled is the one thing a task manager may not do.
///
/// So the fallback stays and the silence goes. Before this, a typo'd `--theme`
/// or a stale `$TASQX_THEME` rendered nord with no hint, which reads as the
/// flag being ignored rather than the name being wrong.
///
/// **A hand-edited `config.toml` is the third case, and it warns too.** It
/// persists like `config set`, but refusing to start would lock the user out of
/// the one tool that can fix the file — the D28 inversion, one config layer
/// over. It was left silent on the theory that `tasqx config` would report it;
/// `tasqx config` was in fact reporting the ignored name as though it were in
/// effect, so nothing in the tool said the name had been dropped. Warning on
/// every command is the point rather than the cost: a persisted bad name is
/// wrong on every run, and stderr keeps stdout scriptable.
///
/// The message, or `None` when there is nothing to say.
///
/// Split from the printing so it is testable at all: the emitting version can
/// only be observed through process-global stderr, and a first version of this
/// was pinned by a test that checked `validate_setting` and that `build_ctx`
/// did not panic — so disabling the warning outright left the suite green.
/// `key` is the setting being resolved, NOT a constant. Hard-coding
/// `"theme.name"` here meant every OTHER setting was validated as a theme name,
/// so `notify.enabled = true` failed that check and was silently replaced by its
/// default — the caller's value dropped, and `config get` reporting `default`.
/// `validate_setting` answers `Ok` for a key with no closed value set, so
/// passing the real key is also what keeps this correct as settings are added.
pub(crate) fn unknown_theme_warning(key: &str, name: &str, source: &str) -> Option<String> {
    validate_setting(key, name).err().map(|_| {
        format!("warning: unknown theme {name:?} from {source}; using the default (try `tasqx theme list`)")
    })
}

/// One setting's value **as it will actually be used**, the layer that supplied
/// it, and the complaint if a layer's value had to be discarded.
///
/// This is the one place the difference between "what a layer said" and "what
/// the tool will do" is resolved, and every reader goes through it — `build_ctx`
/// on the render path, and `config get`/`config list`/`config edit` through
/// `setting_value`. Before it, `config::resolve` was the answer for both, and it
/// only knows precedence: a `config.toml` naming a theme that does not exist was
/// dropped by `theme::load` on the way to the renderer while `config get`
/// happily reported the dropped name. One question, two surfaces, two answers —
/// and the one the user could read was the wrong one.
///
/// **Walks the D9 chain one layer at a time (#197).** The first version called
/// `config::resolve` once — which only knows precedence, not validity — and
/// discarded straight to the default the moment THAT single winning layer
/// failed validation. So `--theme gruvbx` with `gruvbox` sitting right there in
/// `config.toml` reported `nord`/`Source::Default`: a value the user had typed
/// AND persisted, thrown away because a *different, higher* layer had a typo in
/// it. D9's own promise is a chain, and the one thing a chain must get right is
/// what happens when a link misses — it hands off to the next link, not to the
/// ground. The fallback is `s.default` with `Source::Default` only once EVERY
/// layer has been tried and failed: that IS where the value comes from at that
/// point, and crediting `config.toml` for a value it did not supply would be
/// the same lie one field over.
pub(crate) fn effective_setting(
    s: &config::Setting,
    flag: Option<&str>,
    file: Option<&str>,
) -> (String, config::Source, Option<String>) {
    fn pick(v: Option<&str>) -> Option<String> {
        v.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }
    // The env layer is read here, not passed in, for the same reason
    // `config::resolve` reads it internally: a caller cannot forget it.
    let env = s.env.and_then(|e| std::env::var(e).ok());
    let candidates = [
        (pick(flag), config::Source::Flag),
        (pick(env.as_deref()), config::Source::Env),
        (pick(file), config::Source::File),
    ];

    // The highest-precedence layer that supplied a value tasqx could not use —
    // named in the eventual warning regardless of which lower layer rescues
    // it, or of "the default" when none does. Only the first is kept: one
    // warning line per invocation, same as before this walked more than one
    // layer.
    let mut rejected: Option<(String, config::Source)> = None;
    for (value, source) in candidates {
        let Some(value) = value else { continue };
        if validate_setting(s.key, &value).is_ok() {
            let warning = rejected.map(|(name, rej_source)| {
                format!(
                    "warning: unknown theme {name:?} from {}; using {} instead (try `tasqx theme list`)",
                    rej_source.label(s),
                    source.label(s)
                )
            });
            return (value, source, warning);
        }
        rejected.get_or_insert((value, source));
    }

    // Nothing validated: `unknown_theme_warning`'s own wording is exactly this
    // case's message, so build it from the same function rather than a second
    // copy of the sentence.
    let warning = rejected.map(|(name, source)| {
        unknown_theme_warning(s.key, &name, &source.label(s)).expect("already known invalid")
    });
    (s.default.to_string(), config::Source::Default, warning)
}

/// The complaints `config.toml` earns before anything reads a single setting
/// out of it: it does not parse at all, or a `[section] key` names no
/// registered setting (#192).
///
/// Split from the printing exactly like `unknown_theme_warning` is: the
/// emitting version can only be observed through process-global stderr. Two
/// distinct classes, both silent until now: `config::read_table_strict`
/// already existed for `tasqx config`'s OWN reads (`file_value`), but nothing
/// called it from the render path, so every OTHER verb read through the silent
/// `toml_value_in` and never learned the file failed to parse at all. An
/// unknown key is a different failure mode again — `coerce` only rejects a
/// value of the wrong TYPE, so a key nothing declares is simply never looked
/// up, and even `config list` (which reads strictly) had nothing to say about
/// it.
pub(crate) fn config_file_warnings_in(dir: &std::path::Path) -> Vec<String> {
    match config::read_table_strict(dir) {
        Err(e) => vec![e.message],
        Ok(None) => Vec::new(),
        Ok(Some(table)) => table
            .iter()
            .filter_map(|(section, value)| value.as_table().map(|t| (section, t)))
            .flat_map(|(section, inner)| {
                inner.keys().filter_map(move |key| {
                    let dotted = format!("{section}.{key}");
                    (config::find(&dotted).is_none()).then(|| unknown_key(&dotted).message)
                })
            })
            .collect(),
    }
}

/// Warn once, on stderr, about whatever [`config_file_warnings_in`] finds in
/// the user's real `config.toml`.
///
/// Called from `build_ctx`, which already runs before every command (see its
/// own doc comment) — the same "once per process, every verb" treatment the
/// theme-resolution warning gets a few lines below it. Before this, an
/// unclosed `[theme` was reported nowhere but `config get`/`config list`, so
/// `list`, `next` and `add` exited 0 with the config quietly reverted to every
/// default and nothing on stderr to say so.
pub(crate) fn warn_about_config_file() {
    let Some(dir) = config::config_dir() else {
        return;
    };
    for msg in config_file_warnings_in(&dir) {
        eprintln!("warning: {msg}");
    }
}

/// The stems of `themes/*.toml`, sorted. A missing directory is an empty list —
/// built-ins need no files.
///
/// Extracted from `theme list` when `config edit` needed the same list for its
/// picker. Two copies would have let the printed list and the interactive one
/// disagree about which themes exist, and only the interactive one can act on
/// the answer.
pub(crate) fn user_theme_names() -> Vec<String> {
    let Some(dir) = themes_dir() else {
        return Vec::new();
    };
    let mut user: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("toml") {
                p.file_stem().and_then(|x| x.to_str()).map(str::to_string)
            } else {
                None
            }
        })
        .collect();
    user.sort();
    user
}

/// The user's `themes/` directory: `$TASQX_CONFIG_DIR/themes` or the platform
/// config dir. Missing dir is fine — built-ins need no files.
pub(crate) fn themes_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("TASQX_CONFIG_DIR") {
        if !d.is_empty() {
            return Some(PathBuf::from(d).join("themes"));
        }
    }
    directories::ProjectDirs::from("dev", "tasqx", "tasqx")
        .map(|dirs| dirs.config_dir().join("themes"))
}

/// Read `[notify] enabled` from `config.toml` (DESIGN.md §9).
///
/// Native OS toasts are opt-in: absent config means `false`, so every failure
/// mode here — no config dir, no file, malformed TOML, wrong type — lands on
/// "don't notify", never on "notify anyway", and a fresh install is quiet.
pub(crate) fn config_notify_enabled() -> bool {
    let s = config::find("notify.enabled").expect("notify.enabled is a registered setting");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    v == "true"
}

/// Read `[tokens] enabled` from `config.toml` (#17, DESIGN §10).
///
/// Off by default: like [`config_notify_enabled`], every failure mode — no
/// config dir, no file, malformed TOML, wrong type — lands on "don't attribute",
/// so a fresh install never parses AI tool transcripts until the user opts in.
pub(crate) fn config_tokens_enabled() -> bool {
    let s = config::find("tokens.enabled").expect("tokens.enabled is a registered setting");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    v == "true"
}

/// Read `[detail] time_format` from `config.toml`.
///
/// Falls back to `Both` on every failure — no config dir, no file, malformed
/// TOML, or a value the registry would have refused had it come through
/// `config set` — matching how [`config_tokens_enabled`] treats its own failure
/// modes. A hand-edited `config.toml` is the one path that reaches the writer's
/// validation, so this side must not trust what it reads.
pub(crate) fn config_detail_time_format() -> TimeFormat {
    let s = config::find("detail.time_format").expect("detail.time_format is a registered setting");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    match v.as_str() {
        "iso" => TimeFormat::Iso,
        "relative" => TimeFormat::Relative,
        _ => TimeFormat::Both,
    }
}

/// Read `[otlp] enabled` from `config.toml` (#18, DESIGN §10).
///
/// Off by default: like [`config_tokens_enabled`], every failure mode lands on
/// "don't listen", so a fresh install never opens a local telemetry port until
/// the user opts in.
pub(crate) fn config_otlp_enabled() -> bool {
    let s = config::find("otlp.enabled").expect("otlp.enabled is a registered setting");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    v == "true"
}

/// Read `[otlp] port` from `config.toml` (#18), falling back to the registered
/// default (4318). The registry already validated the range, so a parse failure
/// here can only be the default, which is a valid `u16`.
pub(crate) fn config_otlp_port() -> u16 {
    let s = config::find("otlp.port").expect("otlp.port is a registered setting");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    v.parse::<u16>().unwrap_or_else(|_| {
        s.default
            .parse()
            .expect("the registered default is a valid port")
    })
}

/// Read `[daemon] idle_timeout` from `config.toml` (D5): how long the daemon
/// may sit with no clients and no work before it exits by itself.
///
/// Off unless the user asked for it, and every failure mode lands on off — the
/// same direction [`config_notify_enabled`] and [`config_tokens_enabled`] fall
/// in, for a sharper reason: the surprise here is not a missing toast but a
/// background process that vanishes mid-session, and nothing in a daemon's
/// output would explain it after the fact.
pub(crate) fn config_daemon_idle_timeout() -> Option<Duration> {
    let s =
        config::find("daemon.idle_timeout").expect("daemon.idle_timeout is a registered setting");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    idle_timeout_from_minutes(&v)
}

/// The registry's minutes string as the daemon's `Option<Duration>`.
///
/// Split out of [`config_daemon_idle_timeout`] because it is the whole of the
/// decision and the only part testable without a config directory: `0` and
/// anything unparseable are both "never exit". Unparseable is reachable — the
/// writer's range check only covers `config set`, and a hand-edited
/// `idle_timeout = "soon"` reaches here as the default string either way.
pub(crate) fn idle_timeout_from_minutes(value: &str) -> Option<Duration> {
    let minutes = value.trim().parse::<u64>().ok()?;
    (minutes > 0).then(|| Duration::from_secs(minutes * 60))
}

/// `tasqx theme list|show|set`.
///
/// Every arm returns the facts alongside the rendering rather than printing:
/// the theme list, the resolved role→colour map, and the write receipt are all
/// things a script has a real use for — picking a theme from a menu, driving a
/// terminal's own palette from tasqx's, confirming where the value landed.
pub(crate) fn run_theme(ctx: &Ctx, action: &ThemeAction) -> CmdOutcome {
    match action {
        ThemeAction::List => {
            let mut text = String::new();
            text.push_str(&format!("{}\n", ctx.paint("header", "Built-in themes")));
            for name in theme::BUILTINS {
                let marker = if name == ctx.theme.name {
                    " ← active"
                } else {
                    ""
                };
                text.push_str(&format!("  {}{}\n", name, ctx.paint("muted", marker)));
            }
            let mut user_block = Value::Null;
            if let Some(dir) = themes_dir() {
                let user = user_theme_names();
                if !user.is_empty() {
                    text.push_str(&format!("{}\n", ctx.paint("header", "User themes")));
                    text.push_str(&format!(
                        "  {}\n",
                        ctx.paint("muted", &dir.to_string_lossy())
                    ));
                    // Load-and-validate each file rather than merely listing
                    // its stem (#193, completing D46): a `.toml` that cannot
                    // be parsed used to be offered exactly like a working
                    // theme, and the first place a theme author would learn
                    // otherwise was `theme show` or `--theme`, both of which
                    // silently substituted a different theme under the broken
                    // name.
                    let mut broken = Vec::new();
                    for name in &user {
                        if theme::load_reporting(name, Some(&dir))
                            .file
                            .rejection()
                            .is_some()
                        {
                            text.push_str(&format!(
                                "  {name} {}\n",
                                ctx.paint("danger", "(parse error)")
                            ));
                            broken.push(name.clone());
                        } else {
                            text.push_str(&format!("  {name}\n"));
                        }
                    }
                    user_block =
                        json!({ "dir": dir.to_string_lossy(), "names": user, "broken": broken });
                }
            }
            Ok((
                json!({ "active": ctx.theme.name, "builtin": theme::BUILTINS, "user": user_block }),
                text,
            ))
        }
        ThemeAction::Show { name } => {
            // Preview the requested theme (or the active one) at current caps.
            let preview = match name {
                Some(n) => {
                    let resolved = theme::resolve_name(None, None, Some(n));
                    // `theme::load` falls back to the default for a name it does
                    // not know. That is right on the render path — a bad theme
                    // must never fail a task capture — and wrong here, where
                    // showing the user a theme they did not ask for is the whole
                    // failure: a typo'd `gruvbux` printed nord and exited 0, and
                    // nothing distinguished it from a theme that looks like nord.
                    // Validated AFTER resolving so a blank argument still means
                    // "the default", and through the shared validator so this
                    // cannot drift from `theme set` and `config set` the way an
                    // inline copy already did once.
                    validate_setting("theme.name", &resolved)?;
                    // `validate_setting` only checks that a `themes/<name>.toml`
                    // EXISTS, not that it loads — so a broken file passed this
                    // gate and `theme::load` silently substituted the fallback
                    // theme, still labelled `Theme: nord` (#193, completing
                    // D46's "`theme show` treats it as fatal"). `load_reporting`
                    // is what actually knows the difference; refuse the same way
                    // an unknown NAME is refused three lines up, rather than
                    // printing another theme's data under the name that was
                    // asked for.
                    let loaded = theme::load_reporting(&resolved, themes_dir().as_deref());
                    if let Some(reason) = loaded.file.rejection() {
                        return Err(ApiError::bad_request(format!(
                            "theme {resolved:?} could not be loaded: {reason}"
                        )));
                    }
                    Ctx::new(loaded.theme, ctx.caps).with_cols(ctx.cols)
                }
                None => Ctx::new(ctx.theme.clone(), ctx.caps).with_cols(ctx.cols),
            };
            // Block glyphs are Unicode; degrade the swatch to ASCII on the plain/
            // legacy path so `theme show | cat` never emits mojibake.
            let swatch = if preview.caps.unicode {
                "████"
            } else {
                "####"
            };
            let bar = if preview.caps.unicode { "█" } else { "#" };
            let mut text = String::new();
            text.push_str(&format!(
                "{}\n",
                preview.paint("header", &format!("Theme: {}", preview.theme.name))
            ));
            // The resolved role→colour map, built from the SAME `role_names` walk
            // that prints the swatches, so the two views of one theme cannot come
            // to differ about which roles it defines.
            let mut roles = serde_json::Map::new();
            for role in preview.theme.role_names() {
                let sample =
                    preview
                        .theme
                        .paint(&role, &format!("{swatch} sample text"), &preview.caps);
                text.push_str(&format!("  {:<14} {sample}\n", role));
                let st = preview.theme.role(&role);
                roles.insert(
                    role.clone(),
                    json!({
                        "fg": st.fg.map(|c| c.hex()),
                        "bold": st.bold, "dim": st.dim, "underline": st.underline,
                    }),
                );
            }
            // The urgency ramp as the table reads it: one swatch per band, each
            // labelled with the urgency the band starts at (D119). A strip of
            // blended samples previewed a gradient no screen draws. `mono` has
            // no anchors and still has two bands, plain and bold.
            let bands = preview.theme.ramp().len().max(2);
            let starts: Vec<(f64, String)> = (0..bands)
                .map(|i| {
                    let t = i as f64 / (bands - 1) as f64;
                    (t, format!("{:.0}", t * tasqx_core::urgency::DUE_WEIGHT))
                })
                .collect();
            let swatch = bar.repeat(3);
            let strip = starts
                .iter()
                .map(|(t, from)| {
                    format!(
                        "{} {}",
                        preview.theme.ramp_style(*t).paint(&swatch, &preview.caps),
                        preview.paint("muted", from),
                    )
                })
                .collect::<Vec<_>>()
                .join("  ");
            let head = format!("  {:<14} ", "urgency.ramp");
            // Measured unpainted: the escapes are not cells.
            let strip_w = starts
                .iter()
                .map(|(_, from)| render::width(&swatch) + 1 + render::width(from))
                .sum::<usize>()
                + 2 * (starts.len() - 1);
            // The note explains the preview and is not part of it, so it is
            // the thing dropped when the line would otherwise wrap (rule 9).
            let note = format!(
                "({:.0} = as urgent as overdue)",
                tasqx_core::urgency::DUE_WEIGHT
            );
            let fits = render::width(&head) + strip_w + 2 + render::width(&note) <= preview.cols;
            text.push_str(&head);
            text.push_str(&strip);
            if fits {
                text.push_str(&format!("  {}", preview.paint("muted", &note)));
            }
            text.push('\n');
            Ok((
                json!({
                    "name": preview.theme.name,
                    "roles": Value::Object(roles),
                    "ramp": preview.theme.ramp().iter().map(|c| c.hex()).collect::<Vec<_>>(),
                }),
                text,
            ))
        }
        // Delegated, not reimplemented. `theme set X` and `config set theme.name X`
        // are two spellings of ONE write, and spelling them twice is exactly how
        // they came to disagree: validation lived in one and not the other, and
        // then `--json` landed on one and not the other. One function, one shape,
        // by construction rather than by two developers remembering.
        ThemeAction::Set { name } => set_setting("theme.name", name),
    }
}

/// Persist one setting and describe the write. The single implementation behind
/// `tasqx config set <key> <value>` and `tasqx theme set <name>`.
pub(crate) fn set_setting(key: &str, value: &str) -> CmdOutcome {
    let s = config::find(key).ok_or_else(|| unknown_key(key))?;
    validate_setting(s.key, value)?;
    let path = config::write_value(s, value)?;
    let mut text = format!("{} = {}  ({})\n", s.key, value, path.display());
    if let Some(p) = theme_pointer(s.key) {
        text.push_str(&format!("{p}\n"));
    }
    if let Some(w) = otlp_daemon_warning(s.key, value) {
        eprintln!("{w}");
    }
    Ok((
        json!({ "key": s.key, "value": value, "path": path.to_string_lossy() }),
        text,
    ))
}

/// #76.3: `otlp.enabled = true` used to persist with no complaint even when no
/// daemon was running to act on it. The receiver only binds inside `tasqx
/// daemon` (`serve.rs`: `config_otlp_enabled().then(config_otlp_port)`), so a
/// store with the flag on and no daemon looked, from `config get`, identical
/// to one actually receiving — the flag was true either way. Surfaced at
/// set-time rather than left for `config get` to explain later, since that is
/// when the mistake is cheapest to notice and correct.
///
/// Liveness is checked the same way [`open_backend`] decides whether to route
/// a command through a daemon at all: a live connection to the resolved
/// socket. `--socket`/`$TASQX_SOCK` are not in scope here (`config set` takes
/// no socket flag), so this reads the same default/env resolution the
/// daemon-routing path uses when neither is passed.
pub(crate) fn otlp_daemon_warning(key: &str, value: &str) -> Option<String> {
    if key != "otlp.enabled" || value != "true" {
        return None;
    }
    let target = resolve_socket(None);
    if daemon::try_connect(&target).is_some() {
        return None;
    }
    Some(format!(
        "warning: otlp.enabled = true, but no daemon is reachable at {target}; the OTLP/HTTP \
         receiver only binds inside `tasqx daemon`, so nothing will listen until one is running"
    ))
}

/// The live value of a `Home::Store` setting. Read from `core.capabilities`,
/// which already reports `default_project`, so this needs no new API method.
///
/// `Result<Option<_>>` and not `Option<_>`: the two answers "this setting is not
/// set" and "we could not ask the store" are different facts and the caller
/// must be able to tell them apart. The first version was `.ok()?`, which
/// flattened a failed `core.capabilities` call — a dead daemon mid-request, say
/// — into `None`, which every caller then rendered as the empty string. So
/// `config get default_project` answered a transport failure with a blank line
/// and exit 0, and a script reading that value could not tell it from an unset
/// one. A failure is not a value.
pub(crate) fn store_value(be: &mut Backend, key: &str) -> Result<Option<String>, ApiError> {
    let caps = be.call("core.capabilities", &json!({}))?;
    Ok(caps.get(key).and_then(Value::as_str).map(str::to_string))
}

/// One setting's resolved value and the label naming where it came from.
///
/// The ONE answer for all three readers — `config get`, `config list` and the
/// `config edit` snapshot. It was spelled out three times, and the three copies
/// are exactly how a `Home::Store` setting can go missing from one surface
/// while the other two keep reporting it (D30: derive it, do not keep three
/// lists in sync).
pub(crate) fn setting_value(
    store: StoreLookup,
    s: &config::Setting,
    flag: Option<&str>,
) -> Result<(String, String), ApiError> {
    match s.home {
        config::Home::Store => Ok((store(s.key)?.unwrap_or_default(), "store".to_string())),
        config::Home::Toml => {
            // The EFFECTIVE value, never the one a layer asked for and the tool
            // discarded. The warning is dropped here rather than printed:
            // `build_ctx` has already resolved the same setting from the same
            // layers this run and said it once.
            let (v, src, _) = effective_setting(s, flag, file_value(s)?.as_deref());
            Ok((v, src.label(s)))
        }
    }
}

/// Every registered setting, as the rows the interactive screen shows.
///
/// This is the code that decides which settings a user SEES, and it is extracted
/// so a test can run it. Dropping the `Home::Store` arm — so `default_project`
/// silently never appeared on screen — used to leave the whole suite green,
/// because the loop was inline in `run_config_edit`, which needs a real
/// terminal, and every TUI test built its rows by hand.
pub(crate) fn settings_rows(
    store: StoreLookup,
    themes: &[String],
    theme_flag: Option<&str>,
) -> Result<Vec<tui::settings::Row>, ApiError> {
    let mut rows = Vec::new();
    for s in config::SETTINGS {
        let (value, source) = setting_value(store, s, setting_flag_value(s, theme_flag))?;
        rows.push(build_row(s, value, source, themes));
    }
    Ok(rows)
}

/// An unknown key must name the valid ones. Without the list the user's only
/// recourse is to guess, and the registry already knows the answer.
pub(crate) fn unknown_key(key: &str) -> ApiError {
    let valid: Vec<&str> = config::SETTINGS.iter().map(|s| s.key).collect();
    ApiError::bad_request(format!(
        "unknown setting {key:?} (valid: {})",
        valid.join(", ")
    ))
}

/// Read one setting from `config.toml` strictly, reporting a wrong-typed value
/// on stderr before returning the fallback.
///
/// A warning and not an error, on purpose. A malformed file is a parse error
/// because nothing in it can be trusted; a wrong-typed value is one bad line in
/// a file whose other keys still work, and failing the command would break
/// `config list` — the command you run to find exactly this — over that one
/// line. stderr keeps stdout scriptable, so `$(tasqx config get theme.name)`
/// still yields a usable value while the human sees what the file did.
///
/// Every `tasqx config` read goes through here rather than calling
/// `toml_value_strict` directly, so a new read site cannot quietly re-acquire
/// the silence this replaced.
pub(crate) fn file_value(s: &config::Setting) -> Result<Option<String>, ApiError> {
    let read = config::toml_value_strict(s)?;
    if let Some(m) = &read.mismatch {
        eprintln!("warning: {m}");
    }
    Ok(read.value)
}

/// The one-line pointer to the command that makes a theme change visible.
///
/// tasqx's normal output carries only a few coloured accents, so a user who
/// switches themes sees almost nothing change in `tasqx list` and reasonably
/// concludes the write did not take — which is exactly what happened: gruvbox
/// was saved correctly from `config edit` and the user came back asking where
/// they were supposed to notice. `tasqx theme show` prints every role with a
/// swatch and is the only place the choice is obvious.
///
/// One function, three callers (`theme set`, `config set`, `config edit`),
/// because a pointer added to one write path and not the others is precisely
/// how those three drifted apart over validation once already. `None` for every
/// other key: `notify.enabled = true` has nothing to do with themes.
pub(crate) fn theme_pointer(key: &str) -> Option<&'static str> {
    (key == "theme.name").then_some("See it with `tasqx theme show`.")
}

/// Reject a value that would persist but never take effect.
///
/// Shared because the first version put theme validation inline in `theme set`
/// only — so `tasqx theme set bogus` was rejected while
/// `tasqx config set theme.name bogus` wrote it happily and exited 0, and
/// `theme show bogus` previewed the default as if nothing were wrong. The
/// primitive was looser than its own alias, which is backwards: `theme::load`
/// falls back to the default for an unknown name, so the write persists a value
/// that silently does nothing on every run from then on.
/// The CLI-flag layer for one setting. The registry says WHICH settings carry
/// a flag (`Setting::flag`); the invocation supplies that flag's value — today
/// only `--theme` exists, and this is the ONE place it is wired to its
/// setting. Three call sites used to spell the rule as
/// `s.key == "theme.name"` independently — the parallel-list shape D30 exists
/// to kill: a second flag-carrying setting had to be wired into each, and a
/// missed one silently mis-reported the winning layer.
pub(crate) fn setting_flag_value<'a>(
    s: &config::Setting,
    theme_flag: Option<&'a str>,
) -> Option<&'a str> {
    match s.flag {
        Some("--theme") => theme_flag,
        _ => None,
    }
}

pub(crate) fn validate_setting(key: &str, value: &str) -> Result<(), ApiError> {
    // Dispatch on the registry's declared vocabulary, never the key string —
    // config.rs declares `choices: Choices::Themes` precisely so callers do
    // not test keys. Matched on the key, a second Themes-valued setting
    // silently skipped validation: the `theme set bogus` bug this function
    // was created to fix, re-armed.
    let themed = config::find(key).is_some_and(|s| s.choices == config::Choices::Themes);
    if themed {
        let known = theme::BUILTINS.contains(&value)
            || themes_dir().is_some_and(|d| d.join(format!("{value}.toml")).is_file());
        if !known {
            return Err(ApiError::bad_request(format!(
                "unknown theme {value:?} (try `tasqx theme list`)"
            )));
        }
    }
    Ok(())
}

/// `flag` carries the CLI override for the setting being reported — today only
/// `--theme`. Without it `config` reports the file value while the binary
/// renders with the flag's, so the one command whose job is naming the layer
/// that won cannot see the layer that wins most.
pub(crate) fn run_config(
    be: &mut Backend,
    ctx: &Ctx,
    action: &ConfigAction,
    theme_flag: Option<&str>,
) -> CmdOutcome {
    // The flag layer applies per setting — see `setting_flag_value`.
    let flag_for = |s: &config::Setting| -> Option<&str> { setting_flag_value(s, theme_flag) };
    match action {
        ConfigAction::Edit => run_config_edit(be, ctx, theme_flag),
        ConfigAction::Path => {
            let p = config::config_path()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "(no config directory on this platform)".to_string());
            Ok((json!({ "path": p }), format!("{p}\n")))
        }
        ConfigAction::Store => {
            // D74: through a daemon, ask it — `core.capabilities` carries the
            // store the answering engine writes to, which is the one fact D47
            // established a client cannot compute for itself. An older daemon
            // without the field degrades to the pre-D74 output, not an error.
            let daemon_store = if be.remote_socket().is_some() {
                be.call("core.capabilities", &json!({}))
                    .ok()
                    .and_then(|v| v.get("store").and_then(Value::as_str).map(str::to_string))
            } else {
                None
            };
            Ok(store_location(
                be.remote_socket(),
                daemon_store.as_deref(),
                db_path(),
            ))
        }
        ConfigAction::Get { key } => {
            let s = config::find(key).ok_or_else(|| unknown_key(key))?;
            let (value, _) = setting_value(&mut |k| store_value(be, k), s, flag_for(s))?;
            // stdout stays exactly the value, unconditionally — pinned by
            // `config_get_is_silent_about_a_well_typed_value` and
            // `a_wrong_typed_value_stays_silent_outside_config` so
            // `x=$(tasqx config get key)` keeps working. The summary — the
            // same string `config list --json` already carries per row — is
            // additive JSON only, for `--json` callers (#223).
            let text = format!("{value}\n");
            Ok((
                json!({ "key": s.key, "value": value, "summary": s.summary }),
                text,
            ))
        }
        ConfigAction::Describe { key } => {
            let s = config::find(key).ok_or_else(|| unknown_key(key))?;
            let kind = match s.kind {
                config::Kind::Str => "string",
                config::Kind::Bool => "bool",
                config::Kind::Uint => "unsigned integer",
                config::Kind::Minutes => "minutes (0 = never/off)",
            };
            let text = format!(
                "{}\n  {}\n  type: {kind}, default: {}\n",
                s.key, s.summary, s.default
            );
            Ok((
                json!({
                    "key": s.key,
                    "summary": s.summary,
                    "kind": kind,
                    "default": s.default,
                }),
                text,
            ))
        }
        ConfigAction::Set { key, value } => set_setting(key, value),
        ConfigAction::Unset { key } => {
            let s = config::find(key).ok_or_else(|| unknown_key(key))?;
            let existed = config::clear_value(s)?;
            let text = if existed {
                format!("{} unset; now {} (default)\n", s.key, s.default)
            } else {
                format!("{} was not set\n", s.key)
            };
            Ok((json!({ "key": s.key, "removed": existed }), text))
        }
        ConfigAction::List => {
            let mut rows = Vec::new();
            for s in config::SETTINGS {
                let (value, source) = setting_value(&mut |k| store_value(be, k), s, flag_for(s))?;
                rows.push(json!({
                    "key": s.key,
                    "value": value,
                    "source": source,
                    "default": s.default,
                    "home": match s.home {
                        config::Home::Store => "store",
                        config::Home::Toml => "config.toml",
                    },
                    "summary": s.summary,
                }));
            }
            let text = render_config_table(ctx, &rows);
            Ok((json!({ "settings": rows }), text))
        }
    }
}

/// `tasqx config edit` — the interactive settings screen (D26).
///
/// The three pieces this function owns are the three the state machine must not:
/// the TTY gate, the row snapshot (which reads the store and `config.toml`), and
/// the writes. Everything between them is `tui::settings`.
pub(crate) fn run_config_edit(be: &mut Backend, ctx: &Ctx, theme_flag: Option<&str>) -> CmdOutcome {
    // Refuse before a single escape byte is written. Piped, redirected or dumb
    // stdout gets a message on stderr and exit 2, not an alt screen it cannot
    // clear and a command that looks hung.
    if !tui::is_interactive(&ctx.caps) {
        return Err(ApiError::bad_request(
            "`tasqx config edit` needs an interactive terminal (stdout is piped, redirected, \
             or TERM=dumb). Use `tasqx config list` and `tasqx config set <key> <value>`.",
        ));
    }

    // The picker's candidate list. The registry says a setting HAS a closed set
    // and where it comes from; resolving it to values is this layer's job,
    // because it is a filesystem question the state machine must stay free of.
    let mut themes: Vec<String> = theme::BUILTINS.iter().map(|t| t.to_string()).collect();
    for name in user_theme_names() {
        if !themes.contains(&name) {
            themes.push(name);
        }
    }

    let rows = settings_rows(&mut |k| store_value(be, k), &themes, theme_flag)?;

    let mut app = tui::settings::App::new(rows);
    let caps = ctx.caps;
    let saved = tui::with_terminal(|term| settings_loop(term, &mut app, caps, theme_flag))
        .map_err(|e| ApiError::internal(format!("terminal error: {e}")))?;

    // Printed after the alt screen is gone, so the user's scrollback keeps a
    // record of what the session changed — an interactive screen that leaves no
    // trace is impossible to audit afterwards.
    let text = saved_summary(&saved);
    let changed: Vec<Value> = saved
        .iter()
        .map(|(k, v)| json!({ "key": k, "value": v }))
        .collect();
    Ok((json!({ "changed": changed }), text))
}

/// The scrollback record `config edit` leaves behind after the alt screen is
/// gone — an interactive screen that leaves no trace is impossible to audit
/// afterwards.
///
/// Extracted from `run_config_edit` so the theme pointer on this path is
/// reachable from a test at all: the rest of that function needs a real
/// terminal, so the summary was the one piece of it no test could ever see.
pub(crate) fn saved_summary(saved: &[(String, String)]) -> String {
    if saved.is_empty() {
        return "no changes\n".to_string();
    }
    let mut out = String::new();
    for (k, v) in saved {
        out.push_str(&format!("{k} = {v}\n"));
        if let Some(p) = theme_pointer(k) {
            out.push_str(&format!("{p}\n"));
        }
    }
    out
}

/// One screen row for one registered setting.
///
/// Extracted from `run_config_edit`'s snapshot loop so the mapping is reachable
/// from a test. It was inline, and dropping `Home::Store` settings from that
/// loop — so `default_project` silently never reached the screen — left the
/// whole suite green: every TUI test built its rows by hand, so none of them
/// exercised the code that decides which settings a user actually sees.
pub(crate) fn build_row(
    s: &'static config::Setting,
    value: String,
    source: String,
    themes: &[String],
) -> tui::settings::Row {
    tui::settings::Row {
        setting: s,
        value,
        source,
        choices: match s.choices {
            config::Choices::Themes => themes.to_vec(),
            config::Choices::Free => Vec::new(),
            config::Choices::OneOf(values) => values.iter().map(|v| (*v).to_string()).collect(),
            // No candidates, so `begin_edit` falls to its existing "no inline
            // editor — use `tasqx config set`" branch, and the row is still
            // listed with its value and source.
            //
            // Deliberately NOT an ordered multi-select. That would be a fourth
            // interaction mode in a screen D26 kept to two, to save one
            // `tasqx config set dashboard.panels now,next,due` — and the panel
            // order is already discoverable on the dashboard itself, where the
            // numbers are drawn into the panel headings. The write path
            // validates the list either way, which is where a typo actually
            // needs catching.
            config::Choices::ManyOf(_) => Vec::new(),
        },
    }
}

/// The theme name the NEXT frame must be painted in.
///
/// Named and extracted because the live preview is the only reason this screen
/// exists, and nothing proved the loop re-derived it. Hoisting `theme::load`
/// out of the loop body — resolving once instead of per frame, which kills the
/// preview outright — left all 362 tests green: the render test passed a theme
/// in directly, so it proved `render` honours what it is given and nothing
/// about where that came from.
pub(crate) fn frame_theme_name(app: &tui::settings::App, theme_flag: Option<&str>) -> String {
    // A `--theme` flag outranks everything, including a preview: the user asked
    // for that theme for this invocation, and previewing another would be the
    // screen disagreeing with the terminal it is drawn in.
    if let Some(f) = theme_flag.map(str::trim).filter(|f| !f.is_empty()) {
        return f.to_string();
    }
    app.preview_theme()
        .unwrap_or(theme::DEFAULT_THEME)
        .to_string()
}

/// Apply one `Save` action: validate, write, re-resolve, record.
///
/// `write` is injected so a test can observe that the write actually happens.
/// Inline, this whole path could be replaced with a validate-only no-op — so
/// `config edit` changed nothing on disk — with the suite staying green at
/// 362/362. The state machine was thoroughly covered; the twelve lines that
/// turn its decision into a file were not covered at all.
pub(crate) fn apply_save(
    app: &mut tui::settings::App,
    key: &'static str,
    value: &str,
    theme_flag: Option<&str>,
    saved: &mut Vec<(String, String)>,
    mut write: impl FnMut(&'static config::Setting, &str) -> Result<(), ApiError>,
) {
    let s = config::find(key).expect("the screen only names registered settings");
    // The same validator `config set` uses. The picker can only offer valid
    // values today, but a validator applied on one write path and not the other
    // is how `theme set` and `config set` diverged once already.
    match validate_setting(key, value).and_then(|()| write(s, value)) {
        Ok(()) => {
            // Re-resolve rather than assume: a `$TASQX_THEME` or a `--theme`
            // flag still outranks the file we just wrote, and the screen has to
            // say so instead of reporting a change the user's next command will
            // not show.
            let flag = setting_flag_value(s, theme_flag);
            let (v, src) = config::resolve(s, flag, config::toml_value(s).as_deref());
            app.refresh(key, v, src.label(s));
            saved.retain(|(k, _)| k != key);
            saved.push((key.to_string(), value.to_string()));
        }
        Err(e) => app.report_error(e.message),
    }
}

/// Draw, read one key, fold it in, perform whatever the state machine asked for.
///
/// The theme is reloaded from `app.preview_theme()` on EVERY frame, which is
/// what makes the preview live: moving the picker changes what that returns, so
/// the next frame is painted in the candidate theme before anything is written.
pub(crate) fn settings_loop(
    term: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::settings::App,
    caps: Caps,
    theme_flag: Option<&str>,
) -> std::io::Result<Vec<(String, String)>> {
    use ratatui::crossterm::event::{self, Event};

    let dir = themes_dir();
    let mut saved: Vec<(String, String)> = Vec::new();
    loop {
        let name = frame_theme_name(app, theme_flag);
        let active = theme::load(&name, dir.as_deref());
        term.draw(|f| tui::settings::render(app, &active, &caps, f))?;

        // Resize and paste events just redraw; only keys are decisions.
        let Event::Key(key) = event::read()? else {
            continue;
        };
        match app.on_key(key) {
            Some(tui::settings::Action::Quit) => return Ok(saved),
            Some(tui::settings::Action::Save { key, value }) => {
                apply_save(app, key, &value, theme_flag, &mut saved, |s, v| {
                    config::write_value(s, v).map(|_| ())
                });
            }
            None => {}
        }
    }
}

/// One row per setting: key, value, and which layer supplied it. The source
/// column is the point — the question behind a surprising setting is always
/// "which layer won", and a bare value cannot answer it.
pub(crate) fn render_config_table(ctx: &Ctx, rows: &[Value]) -> String {
    // Widths come from the rows about to be printed, floored at the layout this
    // table has always had (D51's rule, one table over). They were plain `18`
    // and `22`, and both are guesses about data the renderer is holding: the
    // registry grew a `daemon.idle_timeout` (19 cells), which overflowed the
    // key column and shoved SOURCE one cell right on that row alone — the
    // misalignment `the_config_table_stays_aligned_when_a_value_is_not_ascii`
    // exists to catch, arriving from the column that was never suspected.
    // Padded, never truncated: a key and a value are the data the reader came
    // for, and this table is where they read it.
    let cells =
        |v: &Value, field: &str| -> usize { render::width(v[field].as_str().unwrap_or("")) };
    let key_w = rows
        .iter()
        .map(|r| cells(r, "key"))
        .max()
        .unwrap_or(0)
        .max(18);
    let val_w = rows
        .iter()
        .map(|r| {
            let w = cells(r, "value");
            // The empty value renders as `(unset)`, which is what has to fit.
            if w == 0 {
                render::width("(unset)")
            } else {
                w
            }
        })
        .max()
        .unwrap_or(0)
        .max(22);
    let mut out = String::new();
    out.push_str(&format!(
        "{}\n",
        ctx.paint(
            "header",
            &format!(
                "{} {} {}",
                render::pad("SETTING", key_w),
                render::pad("VALUE", val_w),
                "SOURCE"
            )
        )
    ));
    for r in rows {
        let key = r["key"].as_str().unwrap_or("");
        let val = r["value"].as_str().unwrap_or("");
        let src = r["source"].as_str().unwrap_or("");
        let shown = if val.is_empty() { "(unset)" } else { val };
        // `render::pad` measures terminal CELLS, not chars, so a value carrying
        // CJK or an emoji — an editor path, a project name — no longer shoves
        // the SOURCE column sideways.
        out.push_str(&format!(
            "{} {} {}\n",
            render::pad(key, key_w),
            render::pad(shown, val_w),
            ctx.paint("muted", src)
        ));
    }
    out
}
