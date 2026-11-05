//! Syntax colors and the structural diff for the diff view. Both sides are parsed as whole files: the old side is
//! the HEAD blob, the new side the working-tree file. Blocking: run off the UI thread.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use git::layout::{Layout, StructSide};
use syntax::structural::{self, Fail};
use syntax::{Highlighted, Lang, Plain};

/// `AGENT_IDE_SYNTAX=off`: no syntax highlighting and no structural diff (escape hatch, and the plain baseline for
/// perf runs).
pub fn syntax_off() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var("AGENT_IDE_SYNTAX").is_ok_and(|v| v == "off"))
}

/// Both versions of the diffed file and their highlights (trees included), kept for the structural pass.
pub struct Sides {
    lang: Lang,
    old: Option<(String, Result<Arc<Highlighted>, Plain>)>,
    new: Option<(String, Result<Arc<Highlighted>, Plain>)>,
}

/// The diff's syntax colors, read from the repo. None when the language is unknown.
pub fn color_diff(
    plain: &Layout,
    repo: &git::Repo,
    file: &git::FileChange,
    cache: &syntax::Cache,
    cancel: &AtomicUsize,
) -> Option<(Option<Layout>, Sides)> {
    let lang = syntax::detect(Path::new(&file.path))?;
    let old = (file.kind != '?')
        .then(|| git::head_text(repo, file.orig.as_deref().unwrap_or(&file.path), syntax::MAX_BYTES))
        .flatten();
    let new = match git::text::read_file(&repo.root.join(&file.path)) {
        Ok(git::text::FileText::Lines { raw, .. }) => Some(raw),
        _ => None,
    };
    Some(color(plain, lang, old, new, cache, cancel))
}

/// Highlights both texts (in parallel, cached by content) and colors the layout's rows; the colored layout is None
/// when neither side could be highlighted.
pub fn color(
    plain: &Layout,
    lang: Lang,
    old: Option<String>,
    new: Option<String>,
    cache: &syntax::Cache,
    cancel: &AtomicUsize,
) -> (Option<Layout>, Sides) {
    // A big file's two parses take about as long as one.
    let (ho, hn) = std::thread::scope(|s| {
        let ho = s.spawn(|| old.as_deref().map(|t| cache.get(lang, t, cancel)));
        let hn = new.as_deref().map(|t| cache.get(lang, t, cancel));
        (ho.join().ok().flatten(), hn)
    });
    let sides = Sides { lang, old: old.zip(ho), new: new.zip(hn) };
    let (so, sn) = (color_side(&sides.old), color_side(&sides.new));
    let colored = (so.is_some() || sn.is_some()).then(|| plain.with_syntax(so.as_ref(), sn.as_ref()));
    (colored, sides)
}

/// The structural diff of `base` (the word layout, colored or not): its changed tokens, moves and split pairing.
pub fn structure(
    base: &Layout,
    sides: &Sides,
    cache: &syntax::StructuralCache,
    cancel: &AtomicUsize,
) -> Result<Layout, Fail> {
    let (Some((ot, oh)), Some((nt, nh))) = (&sides.old, &sides.new) else { return Err(Fail::OneSided) };
    let plain_fail = |p: &Plain| match p {
        Plain::Cancelled => Fail::Cancelled,
        Plain::TooBig => Fail::TooBig,
        Plain::TooSlow => Fail::TooSlow,
        Plain::Failed => Fail::Unsupported,
    };
    let oh = oh.as_ref().map_err(plain_fail)?;
    let nh = nh.as_ref().map_err(plain_fail)?;
    let (ca, cb) = base.changed_lines(ot.split('\n').count(), nt.split('\n').count());
    let s = cache.get(
        sides.lang,
        structural::Side { text: ot, tree: &oh.tree, changed: &ca },
        structural::Side { text: nt, tree: &nh.tree, changed: &cb },
        cancel,
    )?;
    let (so, sn) = (struct_side(ot, &s.old), struct_side(nt, &s.new));
    base.with_structure(&so, &sn).ok_or(Fail::Mismatch)
}

fn color_side(s: &Option<(String, Result<Arc<Highlighted>, Plain>)>) -> Option<git::layout::Side<'_, syntax::Class>> {
    s.as_ref().and_then(|(t, h)| h.as_ref().ok().map(|h| git::layout::Side::new(t, &h.lines)))
}

fn struct_side<'a>(text: &'a str, r: &'a structural::SideResult) -> StructSide<'a> {
    StructSide { lines: text.lines().collect(), marks: &r.marks, partner: &r.partner, moved: &r.moved }
}

#[cfg(test)]
mod tests {
    /// Staged + unstaged edits: every colored row comes from the right side (HEAD for removed lines, the working
    /// tree for added/context lines), so its text is exactly that file line.
    #[test]
    fn diff_colors_come_from_head_and_worktree() {
        use std::process::Command;
        let d = std::env::temp_dir().join(format!("agent-ide-color-diff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&d)
                .status()
                .unwrap()
                .success();
            assert!(ok, "{args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        let head = "fn one() {\n    let a = \"x\";\n}\n\nfn two() {\n\tlet b = 2;\n}\n";
        std::fs::write(d.join("m.rs"), head).unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        std::fs::write(d.join("m.rs"), head.replace("\"x\"", "\"staged\"")).unwrap();
        git(&["add", "."]);
        let work = head.replace("\"x\"", "\"staged\"").replace("let b = 2;", "let b = 3; // unstaged");
        std::fs::write(d.join("m.rs"), &work).unwrap();

        let git::Discovery::Repo(repo) = git::discover(&d) else { panic!("repo") };
        let file = git::status(&repo).unwrap().files.into_iter().find(|f| f.path == "m.rs").unwrap();
        let diff = git::diff(&repo, &file, || false).unwrap();
        let plain = git::layout::layout(&diff);
        let colored = super::color_diff(&plain, &repo, &file, &syntax::Cache::default(), &Default::default()).unwrap().0.unwrap();
        let shown = |t: &str, n: Option<u32>| git::text::display_line(t.lines().nth(n.unwrap() as usize - 1).unwrap(), 0);
        let mut checked = 0;
        for l in colored.lines.iter().filter(|l| l.runs.iter().any(|r| r.class.is_some())) {
            let expect = match l.kind {
                git::LineKind::Del => shown(head, l.old),
                _ => shown(&work, l.new),
            };
            assert_eq!(*l.text, expect, "{:?} row matches its side", l.kind);
            checked += 1;
        }
        assert!(checked >= 4, "colored rows: {checked}");
        let add = colored.lines.iter().find(|l| l.kind == git::LineKind::Add && l.text.contains("unstaged")).unwrap();
        let comment = syntax::Class::Comment as u8;
        assert!(add.runs.iter().any(|r| r.class == Some(comment)), "{:?}", add.runs);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The structural layout of `git diff --no-index` between two Rust texts.
    fn structural_of(name: &str, old: &str, new: &str) -> Layout {
        let d = std::env::temp_dir().join(format!("agent-ide-structural-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a"), old).unwrap();
        std::fs::write(d.join("b"), new).unwrap();
        let raw = std::process::Command::new("git")
            .args(["diff", "--no-index", "--no-color", "a", "b"])
            .current_dir(&d)
            .output()
            .unwrap()
            .stdout;
        let _ = std::fs::remove_dir_all(&d);
        let plain = git::layout::layout(&git::parse_diff(&raw));
        let flag = AtomicUsize::new(0);
        let (colored, sides) = color(&plain, syntax::Lang::Rust, Some(old.into()), Some(new.into()), &syntax::Cache::default(), &flag);
        structure(&colored.unwrap(), &sides, &syntax::StructuralCache::default(), &flag).unwrap()
    }

    #[test]
    fn structural_layout_pairs_by_matched_nodes() {
        // Reformatting: no marks, every removed/added line "same", the old line next to the first new one.
        let old = "fn a() {\n    let x = call(first_argument, second_argument, third_argument);\n}\n";
        let new = "fn a() {\n    let x = call(\n        first_argument,\n        second_argument,\n        third_argument,\n    );\n}\n";
        let l = structural_of("reformat", old, new);
        assert!(l.lines.iter().filter(|x| matches!(x.kind, LineKind::Del | LineKind::Add)).all(|x| x.same));
        let row = l.split.iter().find_map(|r| match *r {
            git::layout::SplitRow::Pair(Some(a), Some(b)) if l.lines[a as usize].kind == LineKind::Del => Some(b),
            _ => None,
        });
        assert_eq!(row.map(|b| &*l.lines[b as usize].text), Some("    let x = call("));

        // A moved function: moved ranges, no changed ones, a hint on each side, moved lines on rows of their own.
        let f = "fn moved(a: u32, b: u32) -> u32 {\n    let c = a + b * 2;\n    c - 1\n}\n";
        let g = "fn other() {\n    println!(\"x\");\n}\n\nfn third() -> bool {\n    true\n}\n";
        let l = structural_of("move", &format!("{f}\n{g}"), &format!("{g}\n{f}"));
        let changed: Vec<_> = l.lines.iter().filter(|x| !x.words.is_empty()).collect();
        assert!(changed.is_empty(), "{changed:?}");
        let hint = |k| l.lines.iter().find(|x| x.kind == k && x.moved.is_some()).and_then(|x| x.moved);
        assert_eq!(hint(LineKind::Del), Some(9), "moved to the new side's line 9");
        assert_eq!(hint(LineKind::Add), Some(1), "moved from line 1");
        for side in [|r: &git::layout::SplitRow| match *r { git::layout::SplitRow::Pair(a, _) => a, _ => None },
            |r: &git::layout::SplitRow| match *r { git::layout::SplitRow::Pair(_, b) => b, _ => None }]
        {
            let order: Vec<u32> = l.split.iter().filter_map(side).collect();
            assert!(order.is_sorted(), "each side keeps its line order: {order:?}");
        }
        assert!(l.split.iter().all(|r| !matches!(*r, git::layout::SplitRow::Pair(Some(a), Some(b))
            if l.lines[a as usize].kind == LineKind::Del && l.lines[b as usize].kind == LineKind::Add)));
    }

    // ---- Fixture set: real commits, structural vs word (`cargo test --release -p agent-ide diff_fixtures --
    // --ignored --nocapture`). Zed (GPL) commits are only referenced: their blobs are read from a local clone at
    // run time (`ZED_GIT_DIR`, default the cargo git checkout's database) and nothing of them is stored here.

    use super::{color, structure};
    use git::LineKind;
    use git::layout::Layout;
    use std::sync::atomic::AtomicUsize;

    /// Tokens a side may mark as changed: `Only` lists them (an identifier entry also matches identifiers that
    /// contain it, e.g. `transform_old` in `test_transform_old`).
    #[derive(Clone, Copy)]
    enum Allow {
        Any,
        Only(&'static [&'static str]),
    }

    const NOTHING: Allow = Allow::Only(&[]);

    struct Fx {
        zed: bool,
        sha: &'static str,
        path: &'static str,
        case: &'static str,
        old: Allow,
        new: Allow,
        /// Some(true): at least one moved block; Some(false): none.
        moves: Option<bool>,
    }

    const fn fx(zed: bool, sha: &'static str, path: &'static str, case: &'static str, old: Allow, new: Allow, moves: Option<bool>) -> Fx {
        Fx { zed, sha, path, case, old, new, moves }
    }

    const FIXTURES: &[Fx] = &[
        // Renames: only the identifier.
        fx(true, "7a6fe73440", "crates/text/src/patch.rs", "rename", Allow::Only(&["transform_old"]), Allow::Only(&["old_to_new"]), Some(false)),
        fx(true, "89e46396f6", "crates/workspace/src/workspace.rs", "rename", Allow::Only(&["visible_panel"]), Allow::Only(&["active_panel"]), Some(false)),
        fx(true, "eb26fb2d45", "crates/editor/src/editor.rs", "rename", Allow::Only(&["text", "text_len"]), Allow::Only(&["lines", "lines_len"]), Some(false)),
        fx(true, "7f70325a93", "crates/language_models/src/provider/bedrock.rs", "rename", Allow::Only(&["handler"]), Allow::Only(&["handle"]), Some(false)),
        fx(true, "719d0f0abf", "crates/editor/src/display_map/tab_map.rs", "rename", Allow::Only(&["skip_leading_tab"]), Allow::Only(&["inside_leading_tab"]), Some(false)),
        fx(true, "3d737fd268", "crates/gpui/src/geometry.rs", "rename (docs)", Allow::Only(&["upper_left"]), Allow::Only(&["top_left"]), Some(false)),
        fx(false, "7af20d3", "crates/app/src/terminal_element.rs", "rename + edits", Allow::Any, Allow::Any, None),
        // Moves: shown as moved, nothing changed inside.
        fx(true, "ec6f2a3ad4", "crates/project/src/project.rs", "move (fn, 4500 lines)", NOTHING, NOTHING, Some(true)),
        fx(true, "7ac055627e", "crates/extensions_ui/src/extensions_ui.rs", "move (impl)", NOTHING, NOTHING, Some(true)),
        fx(true, "b77fab0fae", "crates/gpui2/src/window_input_handler.rs", "move (trait)", NOTHING, NOTHING, Some(true)),
        fx(true, "bf3a5d0a0d", "zed/src/editor/buffer/mod.rs", "move (structs)", NOTHING, NOTHING, Some(true)),
        fx(true, "348760556a", "crates/gpui2/src/key_dispatch.rs", "move (reorder)", NOTHING, NOTHING, Some(true)),
        fx(true, "3c0475d182", "crates/debugger_ui/src/debugger_panel.rs", "move (in a chain)", Allow::Any, Allow::Any, Some(true)),
        fx(true, "6f6e207eb5", "crates/editor/src/mouse_context_menu.rs", "move + edit", Allow::Any, Allow::Any, Some(true)),
        // Reformatting: nothing.
        fx(true, "a829b5be01", "crates/language/src/language.rs", "reformat", NOTHING, NOTHING, Some(false)),
        fx(true, "0ac7a3bc21", "crates/fs/src/fs.rs", "reformat", NOTHING, NOTHING, Some(false)),
        fx(true, "1474429271", "crates/project/src/worktree.rs", "reformat", NOTHING, NOTHING, Some(false)),
        fx(true, "47380001cc", "crates/remote/src/ssh_session.rs", "reformat", NOTHING, NOTHING, Some(false)),
        fx(true, "e0c192d831", "crates/languages/src/vtsls.rs", "reformat (json!)", NOTHING, NOTHING, Some(false)),
        // Added arguments: only the argument.
        fx(true, "101ca00a13", "crates/editor/src/input.rs", "added arg", NOTHING, Allow::Only(&[",", "true"]), Some(false)),
        fx(true, "101ca00a13", "crates/editor/src/navigation.rs", "added arg + rewrap", NOTHING, Allow::Only(&[",", "false"]), Some(false)),
        fx(true, "93b846b2a6", "crates/file_finder/src/file_finder.rs", "added arg + rewrap", NOTHING, Allow::Only(&[",", "action", ".", "include_ignored", ":", "Option", "<", "bool", ">", "or", "(", ")"]), Some(false)),
        fx(true, "897ba9adf2", "crates/markdown/src/html/html_rendering.rs", "added arg", NOTHING, Allow::Only(&[",", "cx", ":", "&", "App"]), Some(false)),
        fx(true, "2d84af91bf", "crates/agent/src/tests/mod.rs", "added arg", Allow::Only(&["_"]), Allow::Only(&[",", "cx"]), Some(false)),
        fx(false, "82c5b79", "crates/app/src/git_panel.rs", "added args + more", Allow::Any, Allow::Any, None),
        // Wrapped expressions: only the wrapper.
        fx(true, "bc81ada227", "crates/multi_buffer/src/multi_buffer.rs", "wrap Some(..)", Allow::Only(&["panic", "!", "(", ")", "\"invalid excerpt id {id:?}\"", "excerpt_locator_for_id"]), Allow::Any, Some(false)),
        fx(true, "f369e9a48b", "crates/vim/src/motion.rs", "wrap Some(..)", Allow::Only(&["(", ")", ",", "{", "}", "DisplayPoint", "from"]), Allow::Any, None),
        fx(true, "954ac0f3ca", "crates/language_models/src/provider/ollama.rs", "wrap in if", NOTHING, Allow::Only(&["if", "settings", ".", "auto_discover", "{", "}"]), Some(false)),
        fx(true, "1b86dbc78b", "crates/git_ui/src/commit_view.rs", "insert in chain", NOTHING, Allow::Only(&[".", "filter", "(", "|", "_", "self", "stash", "is_none", ")"]), Some(false)),
        fx(true, "a658e1d3b0", "crates/git_ui/src/git_panel.rs", "wrap in closure", Allow::Any, Allow::Any, None),
        // Big files, mixed.
        fx(true, "9d50bab893", "crates/git_ui/src/git_panel.rs", "big mixed (9k)", Allow::Any, Allow::Any, None),
        fx(true, "8f0efe9f34", "crates/editor/src/element.rs", "big mixed (13.7k)", Allow::Any, Allow::Any, None),
    ];

    fn run_git(repo: &std::path::Path, args: &[&str]) -> Option<Vec<u8>> {
        let out = std::process::Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
        out.status.success().then_some(out.stdout)
    }

    /// Tokens (words, single other characters; strings/whole comments are not split here) of marked texts.
    fn tokens(s: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut word = false;
        for ch in s.chars() {
            let w = ch.is_alphanumeric() || ch == '_';
            if ch.is_whitespace() {
                word = false;
            } else if w && word {
                out.last_mut().unwrap().push(ch);
            } else {
                out.push(ch.to_string());
                word = w;
            }
        }
        out
    }

    /// Changed text per side as the view shows it: word engine = word ranges, or the whole line when there are none;
    /// structural = the changed ranges.
    fn marked(l: &Layout, kind: LineKind) -> Vec<String> {
        l.lines
            .iter()
            .filter(|x| x.kind == kind)
            .flat_map(|x| {
                if l.engine == git::layout::Engine::Word && x.words.is_empty() {
                    vec![x.text.trim().to_string()]
                } else {
                    x.words.iter().map(|r| x.text[r.start as usize..r.end as usize].to_string()).collect()
                }
            })
            .filter(|t| !t.is_empty())
            .collect()
    }

    /// Tokens outside the allowed list (empty = ok). Strings are atoms in the structural diff, so a changed string
    /// counts as allowed when its full text is listed.
    fn violations(texts: &[String], allow: Allow) -> Vec<String> {
        let Allow::Only(ok) = allow else { return Vec::new() };
        let fine = |t: &str| ok.iter().any(|a| t == *a || (a.chars().all(|c| c.is_alphanumeric() || c == '_') && t.contains(a)));
        texts
            .iter()
            .filter(|t| !fine(t.trim()))
            .flat_map(|t| tokens(t))
            .filter(|t| !fine(t) && !ok.iter().any(|a| a.contains(t.as_str()) && a.starts_with('"')))
            .collect()
    }

    struct Stats {
        lines: usize,
        bytes: usize,
        moved: usize,
        quiet: usize,
        paired: usize,
        pass: bool,
        why: String,
    }

    fn stats(l: &Layout, f: &Fx) -> Stats {
        let changed = |x: &git::layout::DLine| {
            matches!(x.kind, LineKind::Del | LineKind::Add)
                && (if l.engine == git::layout::Engine::Word { true } else { !x.words.is_empty() })
        };
        let bytes = l
            .lines
            .iter()
            .filter(|x| changed(x))
            .map(|x| if x.words.is_empty() { x.text.trim().len() } else { x.words.iter().map(|r| r.len()).sum() })
            .sum();
        let paired = l
            .split
            .iter()
            .filter(|r| matches!(**r, git::layout::SplitRow::Pair(Some(a), Some(b)) if l.lines[a as usize].kind == LineKind::Del && l.lines[b as usize].kind == LineKind::Add))
            .count();
        let moved_blocks = l.lines.iter().filter(|x| x.moved.is_some()).count();
        let (vo, vn) = (violations(&marked(l, LineKind::Del), f.old), violations(&marked(l, LineKind::Add), f.new));
        let mut why = String::new();
        if !vo.is_empty() {
            why += &format!("old {:?} ", &vo[..vo.len().min(6)]);
        }
        if !vn.is_empty() {
            why += &format!("new {:?} ", &vn[..vn.len().min(6)]);
        }
        match f.moves {
            Some(true) if moved_blocks == 0 => why += "no move ",
            Some(false) if moved_blocks > 0 => why += &format!("{moved_blocks} spurious moves "),
            _ => {}
        }
        Stats {
            lines: l.lines.iter().filter(|x| changed(x)).count(),
            bytes,
            moved: l.lines.iter().filter(|x| !x.moves.is_empty()).count(),
            quiet: l.lines.iter().filter(|x| x.same).count(),
            paired,
            pass: why.is_empty(),
            why,
        }
    }

    #[test]
    #[ignore]
    fn diff_fixtures() {
        let zed = std::env::var("ZED_GIT_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".cargo/git/db/zed-a70e2ad075855582")
        });
        let ours = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let (cache, scache, flag) = (syntax::Cache::default(), syntax::StructuralCache::default(), AtomicUsize::new(0));
        println!("| fixture | case | word: lines / bytes / ok | structural: lines / bytes / moved / quiet / ok | paired rows w→s | ms | notes |");
        println!("|---|---|---|---|---|---|---|");
        let (mut ok_w, mut ok_s) = (0, 0);
        for f in FIXTURES {
            let repo = if f.zed { zed.as_path() } else { ours.as_path() };
            let parent = format!("{}^", f.sha);
            let (Some(old), Some(new), Some(raw)) = (
                run_git(repo, &["show", &format!("{parent}:{}", f.path)]),
                run_git(repo, &["show", &format!("{}:{}", f.sha, f.path)]),
                run_git(repo, &["diff", "--no-color", "--no-ext-diff", &parent, f.sha, "--", f.path]),
            ) else {
                println!("| {} {} | {} | missing | | | | |", f.sha, f.path, f.case);
                continue;
            };
            let (old, new) = (String::from_utf8(old).unwrap(), String::from_utf8(new).unwrap());
            let plain = git::layout::layout(&git::parse_diff(&raw));
            let lang = syntax::detect(std::path::Path::new(f.path)).unwrap();
            let (colored, sides) = color(&plain, lang, Some(old), Some(new), &cache, &flag);
            let word = colored.unwrap_or(plain);
            let t = std::time::Instant::now();
            let structural = structure(&word, &sides, &scache, &flag);
            let ms = t.elapsed().as_secs_f64() * 1e3;
            let w = stats(&word, f);
            ok_w += w.pass as usize;
            let name = f.path.rsplit('/').next().unwrap();
            let s_col = match &structural {
                Ok(s) => {
                    let s = stats(s, f);
                    ok_s += s.pass as usize;
                    let row = format!(
                        "{} / {} / {} / {} / {} | {} → {} | {ms:.0} | {}",
                        s.lines, s.bytes, s.moved, s.quiet, if s.pass { "✓" } else { "✗" }, w.paired, s.paired, s.why
                    );
                    row
                }
                Err(e) => format!("fallback {e:?} | | {ms:.0} | "),
            };
            println!(
                "| {} {name} | {} | {} / {} / {} | {s_col} |",
                f.sha,
                f.case,
                w.lines,
                w.bytes,
                if w.pass { "✓" } else { "✗" }
            );
        }
        println!("\nexpectation met: word {ok_w}/{n}, structural {ok_s}/{n}", n = FIXTURES.len());
    }

    /// Timing (release): `STRUCT_BENCH=<file> cargo test --release -p agent-ide struct_bench -- --ignored
    /// --nocapture`. Renames `self.` → `this.` on 1 / 50 / 500 evenly spaced lines and times the passes.
    #[test]
    #[ignore]
    fn struct_bench() {
        let path = std::env::var("STRUCT_BENCH").expect("STRUCT_BENCH=<file>");
        let old = std::fs::read_to_string(&path).unwrap();
        let lang = syntax::detect(std::path::Path::new(&path)).unwrap();
        let flag = AtomicUsize::new(0);
        let candidates: Vec<usize> = old.lines().enumerate().filter(|(_, l)| l.contains("self.")).map(|(i, _)| i).collect();
        for edits in [1usize, 50, 500] {
            let step = (candidates.len() / edits).max(1);
            let pick: std::collections::HashSet<usize> = candidates.iter().step_by(step).take(edits).copied().collect();
            let new: String = old
                .split_inclusive('\n')
                .enumerate()
                .map(|(i, l)| if pick.contains(&i) { l.replacen("self.", "this.", 1) } else { l.to_string() })
                .collect();
            let d = std::env::temp_dir().join(format!("agent-ide-struct-bench-{}", std::process::id()));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("a"), &old).unwrap();
            std::fs::write(d.join("b"), &new).unwrap();
            let raw = std::process::Command::new("git")
                .args(["diff", "--no-index", "--no-color", "a", "b"])
                .current_dir(&d)
                .output()
                .unwrap()
                .stdout;
            let _ = std::fs::remove_dir_all(&d);
            let plain = git::layout::layout(&git::parse_diff(&raw));
            let t = std::time::Instant::now();
            let (colored, sides) = color(&plain, lang, Some(old.clone()), Some(new), &syntax::Cache::default(), &flag);
            let color_ms = t.elapsed().as_secs_f64() * 1e3;
            let Some(base) = colored else {
                println!("{path}: not highlighted within the syntax caps ({color_ms:.0} ms): stays plain, word diff");
                return;
            };
            let t = std::time::Instant::now();
            let s = structure(&base, &sides, &syntax::StructuralCache::default(), &flag);
            let ms = t.elapsed().as_secs_f64() * 1e3;
            let marked = s.as_ref().map_or(0, |l| l.lines.iter().filter(|x| !x.words.is_empty()).count());
            println!(
                "{path}: {} lines, {edits} edits ({} rows): parse+color {color_ms:.0} ms, structural {ms:.1} ms ({}, {marked} marked rows)",
                old.lines().count(),
                base.lines.len(),
                s.as_ref().map_or_else(|e| format!("{e:?}"), |_| "ok".into()),
            );
        }
    }
}
