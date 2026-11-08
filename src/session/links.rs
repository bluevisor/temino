use super::*;

/// The command line that replaces the running shell with `shell` (its
/// path) in place, integration and all: `exec env … fish --init-command …`.
/// Its words are quoted so bash, zsh and fish all read them the same.
pub fn exec_line(shell: &str) -> std::io::Result<String> {
    let mut env = Vec::new();
    let argv = integrate(shell, &mut env)?;
    let quote = |word: &String| {
        if !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=+,@%".contains(c)) {
            word.clone()
        } else {
            // Backslashes differ inside fish's single quotes; in double
            // quotes `\\` and `'` read alike everywhere.
            format!("'{}'", word.replace('\\', r#"'"\\"'"#).replace('\'', r#"'"'"'"#))
        }
    };
    let words: Vec<String> = env.iter().chain(&argv).map(quote).collect();
    Ok(format!("exec env {}", words.join(" ")))
}

/// Writes `shell`'s integration script to the settings directory and
/// returns the command line that loads it (adding to `env` what that
/// needs). Shells Temino doesn't know are started like bash.
pub(super) fn integrate(shell: &str, env: &mut Vec<String>) -> std::io::Result<Vec<String>> {
    let dir = crate::settings_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(match Kind::of_path(shell) {
        Some(Kind::Zsh) => {
            // zsh has no --rcfile: point ZDOTDIR at a .zshenv that puts the
            // user's back and then loads the integration.
            let zdotdir = format!("{dir}/zsh");
            std::fs::create_dir_all(&zdotdir)?;
            std::fs::write(format!("{zdotdir}/.zshenv"), ZSH_ENV)?;
            std::fs::write(format!("{zdotdir}/temino.zsh"), ZSH_INTEGRATION)?;
            if let Ok(original) = std::env::var("ZDOTDIR") {
                env.push(format!("TEMINO_ZDOTDIR={original}"));
            }
            env.push(format!("ZDOTDIR={zdotdir}"));
            vec![shell.into(), "-l".into(), "-i".into()]
        }
        Some(Kind::Fish) => {
            let path = format!("{dir}/temino.fish");
            std::fs::write(&path, FISH_INTEGRATION)?;
            // Through the environment, so the path needs no fish quoting.
            env.push(format!("TEMINO_INTEGRATION={path}"));
            let mut argv = vec![shell.to_owned()];
            argv.extend(["--login", "--interactive", "--init-command", "source $TEMINO_INTEGRATION"].map(String::from));
            argv
        }
        Some(Kind::Bash) | None => {
            let path = format!("{dir}/temino.bash");
            std::fs::write(&path, BASH_INTEGRATION)?;
            vec![shell.into(), "--rcfile".into(), path, "-i".into()]
        }
    })
}

/// The checked-out branch of the repository containing `path`, read
/// straight from .git/HEAD (no git process).
/// A link in a block's output, and the cells it covers as
/// (output line, first column, last column) runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub url: String,
    pub cells: Vec<(usize, usize, usize)>,
}

/// The path-like word around `index` in `chars`, without quotes or
/// brackets around it, a `:line:column` after it, or sentence punctuation.
pub fn find_path(chars: &[char], index: usize) -> Option<(usize, usize)> {
    let stop = |c: char| {
        c.is_whitespace()
            || matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '|' | ',' | ';')
            || ('\u{2500}'..='\u{259f}').contains(&c)
    };
    if stop(*chars.get(index)?) {
        return None;
    }
    let mut start = index;
    while start > 0 && !stop(chars[start - 1]) {
        start -= 1;
    }
    let mut end = index + 1;
    while end < chars.len() && !stop(chars[end]) {
        end += 1;
    }
    // `src/main.rs:12:5` → `src/main.rs`.
    loop {
        let digits = chars[start..end].iter().rev().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && end - digits > start && chars[end - digits - 1] == ':' {
            end -= digits + 1;
        } else if end > start && matches!(chars[end - 1], '.' | ':' | '!' | '?') {
            end -= 1;
        } else {
            break;
        }
    }
    (end > start && index < end).then_some((start, end))
}

/// Where URLs sit in `chars`, as [start, end) character ranges.
pub fn find_urls(chars: &[char]) -> Vec<(usize, usize)> {
    const SCHEMES: [&str; 6] = ["https://", "http://", "file://", "ftp://", "ssh://", "mailto:"];
    let allowed = |c: char| {
        (c.is_ascii_graphic() && !matches!(c, '<' | '>' | '"' | '`' | '{' | '}' | '|' | '^' | '\\'))
            || (!c.is_ascii() && c.is_alphanumeric())
    };
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let boundary = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
        let scheme = SCHEMES.iter().find(|scheme| {
            let scheme: Vec<char> = scheme.chars().collect();
            chars[i..].starts_with(&scheme)
        });
        let Some(scheme) = scheme.filter(|_| boundary) else {
            i += 1;
            continue;
        };
        let body = i + scheme.len();
        let mut end = body;
        while end < chars.len() && allowed(chars[end]) {
            end += 1;
        }
        // Trailing punctuation belongs to the sentence, and a closing
        // bracket to the text around it unless the URL opened one.
        while end > body {
            let last = chars[end - 1];
            let opened = |open: char| chars[i..end].iter().filter(|&&c| c == open).count();
            let closed = |close: char| chars[i..end].iter().filter(|&&c| c == close).count();
            let unbalanced = match last {
                ')' => closed(')') > opened('('),
                ']' => closed(']') > opened('['),
                _ => false,
            };
            if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*') || unbalanced {
                end -= 1;
            } else {
                break;
            }
        }
        if end > body {
            found.push((i, end));
        }
        i = end.max(i + 1);
    }
    found
}

pub fn git_branch(path: &str) -> Option<String> {
    let mut dir = std::path::PathBuf::from(path);
    loop {
        let git = dir.join(".git");
        if git.is_dir() {
            let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
            let head = head.trim();
            return Some(match head.strip_prefix("ref: refs/heads/") {
                Some(branch) => branch.to_owned(),
                None => head.chars().take(7).collect(),
            });
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// A UTF-8 `LANG` for the shell when nothing in `env` picks a locale.
/// Opened from the Dock or Finder, a macOS app gets no LANG at all, and the
/// shell and its programs fall back to ASCII: pasted Chinese turns into
/// escapes. Uses the system's region, as Terminal does.
pub(super) fn utf8_lang(env: &[String]) -> Option<String> {
    let set = |key: &str| {
        env.iter().any(|entry| entry.strip_prefix(key).is_some_and(|rest| rest.starts_with('=') && rest.len() > 1))
    };
    if set("LANG") || set("LC_ALL") || set("LC_CTYPE") {
        return None;
    }
    if cfg!(target_os = "haiku") {
        return None;
    }
    if cfg!(target_os = "macos") {
        static MAC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        return Some(MAC.get_or_init(mac_lang).clone());
    }
    Some("C.UTF-8".to_owned())
}

/// The system region's UTF-8 locale.
pub(super) fn mac_lang() -> String {
    let region = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLocale"])
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    // "en_US@rg=gbzzzz" → "en_US.UTF-8", if the system has that locale.
    let lang = region
        .map(|region| format!("{}.UTF-8", region.split('@').next().unwrap_or("")))
        .filter(|lang| lang.len() > 6 && std::path::Path::new("/usr/share/locale").join(lang).is_dir());
    lang.unwrap_or_else(|| "en_US.UTF-8".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        find_urls(&chars).into_iter().map(|(a, b)| chars[a..b].iter().collect()).collect()
    }

    fn path(text: &str, at: usize) -> Option<String> {
        let chars: Vec<char> = text.chars().collect();
        find_path(&chars, at).map(|(a, b)| chars[a..b].iter().collect())
    }

    #[test]
    fn every_scheme_is_found_and_several_on_a_line() {
        assert_eq!(
            urls("a http://x.io b ftp://f.org/p c ssh://git@h:22/r d mailto:me@x.io e file:///etc/hosts"),
            ["http://x.io", "ftp://f.org/p", "ssh://git@h:22/r", "mailto:me@x.io", "file:///etc/hosts"]
        );
        assert_eq!(urls("no links here"), Vec::<String>::new());
        assert_eq!(urls(""), Vec::<String>::new());
    }

    #[test]
    fn urls_stop_at_quotes_brackets_and_markdown() {
        assert_eq!(urls("\"https://x.io/a\""), ["https://x.io/a"]);
        assert_eq!(urls("<https://x.io/a>"), ["https://x.io/a"]);
        assert_eq!(urls("'https://x.io/a'"), ["https://x.io/a"]);
        assert_eq!(urls("**https://x.io/a**"), ["https://x.io/a"]);
        assert_eq!(urls("[https://x.io/a]"), ["https://x.io/a"]);
        assert_eq!(urls("`https://x.io/a`"), ["https://x.io/a"]);
        assert_eq!(urls("https://x.io/a?q=1&r=2#frag!"), ["https://x.io/a?q=1&r=2#frag"]);
        assert_eq!(urls("https://x.io/list[0]"), ["https://x.io/list[0]"]);
    }

    #[test]
    fn urls_keep_non_ascii_letters_and_need_a_word_boundary() {
        assert_eq!(urls("見て https://例え.jp/パス。"), ["https://例え.jp/パス"]);
        assert_eq!(urls("(https://x.io)"), ["https://x.io"]);
        assert_eq!(urls("xhttp://x.io 1http://y.io"), Vec::<String>::new());
        assert_eq!(urls("-https://x.io"), ["https://x.io"]);
        // A scheme and nothing else is not a link.
        assert_eq!(urls("mailto: http://."), Vec::<String>::new());
    }

    #[test]
    fn url_ranges_are_in_characters_not_bytes() {
        let chars: Vec<char> = "中文 https://x.io".chars().collect();
        assert_eq!(find_urls(&chars), [(3, 15)]);
    }

    #[test]
    fn paths_lose_line_numbers_and_sentence_punctuation() {
        assert_eq!(path("src/main.rs:12", 2).as_deref(), Some("src/main.rs"));
        assert_eq!(path("src/main.rs:12:", 2).as_deref(), Some("src/main.rs"));
        assert_eq!(path("see ./build.sh.", 6).as_deref(), Some("./build.sh"));
        assert_eq!(path("is it /tmp/x?", 8).as_deref(), Some("/tmp/x"));
        assert_eq!(path("done: ~/a!", 7).as_deref(), Some("~/a"));
        assert_eq!(path("(src/lib.rs)", 3).as_deref(), Some("src/lib.rs"));
        assert_eq!(path("{a,b/c.txt}", 4).as_deref(), Some("b/c.txt"));
        assert_eq!(path("x|y/z", 3).as_deref(), Some("y/z"));
        assert_eq!(path("файл/путь.txt", 0).as_deref(), Some("файл/путь.txt"));
    }

    #[test]
    fn paths_need_a_word_under_the_click() {
        assert_eq!(path("", 0), None);
        assert_eq!(path("abc", 3), None);
        assert_eq!(path("abc", 99), None);
        assert_eq!(path("a (b)", 2), None);
        assert_eq!(path(":12", 1), None);
        assert_eq!(path("...", 1), None);
        assert_eq!(path("│", 0), None);
    }

    #[test]
    fn utf8_lang_is_left_alone_when_a_locale_is_set() {
        let env = |entries: &[&str]| entries.iter().map(|e| e.to_string()).collect::<Vec<_>>();
        assert_eq!(utf8_lang(&env(&["LANG=fr_FR.UTF-8"])), None);
        assert_eq!(utf8_lang(&env(&["HOME=/x", "LC_ALL=C"])), None);
        assert_eq!(utf8_lang(&env(&["LC_CTYPE=UTF-8"])), None);
        if cfg!(target_os = "haiku") {
            return;
        }
        // Empty, or only a look-alike name: still no locale.
        for entries in [&[][..], &["LANG="], &["LANGUAGE=en"], &["MY_LANG=en_US.UTF-8"], &["LC_ALLX=C"]] {
            let lang = utf8_lang(&env(entries)).unwrap_or_default();
            assert!(lang.ends_with(".UTF-8"), "{entries:?} gave {lang:?}");
        }
    }
}
