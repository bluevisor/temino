//! Programs that are safe to run again as they were typed: they show
//! things, attach to something that outlived the quit, or open a file.
//! Dev servers too, under their own setting.

use super::*;

/// System and GPU monitors.
const MONITORS: [&str; 16] = [
    "top", "htop", "btop", "btm", "nvtop", "glances", "atop", "gtop", "gotop", "bpytop", "bashtop", "zenith", "s-tui",
    "nvitop", "macmon", "ctop",
];

/// Full-screen browsers and pagers: nothing happens until you ask.
const VIEWERS: [&str; 16] = [
    "k9s",
    "lazygit",
    "lazydocker",
    "tig",
    "gitui",
    "yazi",
    "ranger",
    "lf",
    "nnn",
    "broot",
    "mc",
    "less",
    "more",
    "most",
    "man",
    "ncdu",
];

const EDITORS: [&str; 13] =
    ["vim", "nvim", "vi", "view", "hx", "helix", "nano", "micro", "emacs", "kak", "ne", "joe", "vis"];

/// Package scripts that serve or watch, run as `npm run <script>`,
/// `pnpm <script>`.
const DEV_SCRIPTS: [&str; 8] = ["dev", "start", "serve", "watch", "preview", "develop", "storybook", "server"];

/// Dev servers, by program, and the words that make a run of it one: the
/// first must be its first argument that isn't a flag, the rest follow in
/// order. No words: any run of it.
const SERVERS: &[(&str, &[&[&str]])] = &[
    ("next", &[&["dev"], &["start"]]),
    ("vite", &[]),
    ("nuxt", &[&["dev"]]),
    ("nuxi", &[&["dev"]]),
    ("astro", &[&["dev"], &["preview"]]),
    ("remix", &[&["dev"]]),
    ("ng", &[&["serve"]]),
    ("expo", &[&["start"]]),
    ("turbo", &[&["dev"], &["run", "dev"]]),
    ("nx", &[&["serve"]]),
    ("wrangler", &[&["dev"]]),
    ("vercel", &[&["dev"]]),
    ("netlify", &[&["dev"]]),
    ("hugo", &[&["server"], &["serve"]]),
    ("jekyll", &[&["serve"], &["s"]]),
    ("rails", &[&["server"], &["s"]]),
    ("flask", &[&["run"]]),
    ("streamlit", &[&["run"]]),
    ("uvicorn", &[]),
    ("gunicorn", &[]),
    ("nodemon", &[]),
    ("air", &[]),
    ("trunk", &[&["serve"]]),
    ("mix", &[&["phx.server"]]),
    ("deno", &[&["task", "dev"], &["task", "start"]]),
    ("cargo", &[&["watch"], &["leptos", "watch"]]),
    ("php", &[&["artisan", "serve"]]),
    ("python", &[&["manage.py", "runserver"]]),
    ("python3", &[&["manage.py", "runserver"]]),
    ("docker", &[&["compose", "up"]]),
    ("podman", &[&["compose", "up"]]),
    ("docker-compose", &[&["up"]]),
];

/// Modules `python -m` runs as servers.
const PYTHON_SERVERS: [&str; 4] = ["http.server", "uvicorn", "flask", "streamlit"];

/// `command` again, or the line that reattaches what it ran, when it's
/// safe to: never through sudo (it'd stop at a password prompt), nor in a
/// pipeline, a list, with a redirect or a variable.
pub(super) fn line(command: &str) -> Option<(Kind, String)> {
    if command.contains(['|', ';', '&', '`', '$', '>', '<', '(']) {
        return None;
    }
    let words = shell_words(command);
    let at = words.iter().position(|word| {
        !(word.starts_with('-') || word.contains('=') || agent::LAUNCHERS.contains(word) || *word == "dlx")
    })?;
    if words[..at].iter().any(|word| matches!(*word, "sudo" | "doas")) {
        return None;
    }
    let name = words[at].rsplit('/').next().unwrap_or(words[at]);
    let args = &words[at + 1..];
    let program = |line: String| Some((Kind::Program, line));
    let as_typed = || program(command.to_owned());
    match name {
        _ if MONITORS.contains(&name) || VIEWERS.contains(&name) || EDITORS.contains(&name) => as_typed(),
        "tmux" => tmux(command, args).and_then(program),
        "zellij" => zellij(command, args).and_then(program),
        "screen" => screen(command, args).and_then(program),
        "ssh" | "mosh" if remote_shell(name, args) => as_typed(),
        "tail" | "journalctl" if follows(args) => as_typed(),
        "stern" => as_typed(),
        "docker" | "podman" | "kubectl" | "oc" | "docker-compose" if args.contains(&"logs") && follows(args) => {
            as_typed()
        }
        "npm" | "pnpm" | "yarn" | "bun" => {
            let mut positional = args.iter().filter(|word| !word.starts_with('-'));
            let script = match positional.next()? {
                &"run" | &"run-script" => positional.next()?,
                script => script,
            };
            DEV_SCRIPTS.contains(script).then(|| (Kind::Server, command.to_owned()))
        }
        "python" | "python3" if args.windows(2).any(|pair| pair[0] == "-m" && PYTHON_SERVERS.contains(&pair[1])) => {
            Some((Kind::Server, command.to_owned()))
        }
        _ => {
            let (_, ways) = SERVERS.iter().find(|(server, _)| *server == name)?;
            let serves = ways.is_empty() || ways.iter().any(|way| runs_as(args, way));
            serves.then(|| (Kind::Server, command.to_owned()))
        }
    }
}

/// Whether `args` starts (past flags) with `way`'s first word, then has
/// the rest in order.
fn runs_as(args: &[&str], way: &[&str]) -> bool {
    let mut positional = args.iter().filter(|word| !word.starts_with('-'));
    if positional.next() != way.first() {
        return false;
    }
    way[1..].iter().all(|word| positional.any(|arg| arg == word))
}

/// A follow flag: `-f`, `-F`, `--follow`, or one in a cluster (`-fn 50`).
fn follows(args: &[&str]) -> bool {
    args.iter().any(|arg| {
        arg.starts_with("--follow") || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains(['f', 'F']))
    })
}

/// tmux reattaches: an attach as typed; a named new session as attach-or-
/// create; a bare `tmux` (its session outlives the quit) as `tmux attach`.
fn tmux(command: &str, args: &[&str]) -> Option<String> {
    let mut positional = args.iter().filter(|word| !word.starts_with('-'));
    match positional.next() {
        Some(&("attach" | "attach-session" | "a" | "at")) => Some(command.to_owned()),
        Some(&("new" | "new-session")) if args.contains(&"-s") => {
            Some(if args.contains(&"-A") { command.to_owned() } else { format!("{command} -A") })
        }
        None | Some(&("new" | "new-session")) => Some("tmux attach".to_owned()),
        _ => None,
    }
}

/// zellij reattaches: an attach as typed, a named session by its name,
/// else the one there is.
fn zellij(command: &str, args: &[&str]) -> Option<String> {
    if matches!(args.iter().find(|word| !word.starts_with('-')), Some(&("attach" | "a"))) {
        return Some(command.to_owned());
    }
    let name = args.iter().position(|word| matches!(*word, "-s" | "--session")).and_then(|at| args.get(at + 1));
    match name {
        Some(name) => Some(format!("zellij attach {name}")),
        None if args.iter().all(|word| word.starts_with('-')) => Some("zellij attach".to_owned()),
        None => None,
    }
}

/// screen reattaches: `-r`/`-x`/`-R` as typed; a bare `screen` (detached
/// when its terminal went) as `screen -r`.
fn screen(command: &str, args: &[&str]) -> Option<String> {
    if args.iter().any(|arg| matches!(*arg, "-r" | "-x" | "-R" | "-RR" | "-DR" | "-dr")) {
        Some(command.to_owned())
    } else if args.is_empty() {
        Some("screen -r".to_owned())
    } else {
        None
    }
}

/// An interactive login, not a command run remotely: past its options
/// (and their values), only the host.
fn remote_shell(program: &str, args: &[&str]) -> bool {
    // ssh's options that take a value, and mosh's.
    let takes_value = |flag: &str| match program {
        "ssh" => flag.len() == 2 && "BbcDEeFIiJLlmOoPpQRSWw".contains(&flag[1..]),
        _ => matches!(flag, "-p" | "--port"),
    };
    let mut hosts = 0;
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if *word == "--" {
            return false;
        } else if word.starts_with('-') {
            if takes_value(word) {
                words.next();
            }
        } else {
            hosts += 1;
        }
    }
    hosts == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(command: &str) -> Option<Kind> {
        line(command).map(|(kind, _)| kind)
    }

    fn again(command: &str) -> Option<String> {
        line(command).map(|(_, line)| line)
    }

    #[test]
    fn shows_and_editors_run_again_as_typed() {
        for command in
            ["btop", "htop -d 10", "/usr/bin/top -o cpu", "lazygit", "k9s -n prod", "man tmux", "nvim src/main.rs"]
        {
            assert_eq!(line(command), Some((Kind::Program, command.to_owned())), "{command}");
        }
        assert_eq!(again("tail -fn 50 log.txt").as_deref(), Some("tail -fn 50 log.txt"));
        assert_eq!(again("journalctl -u nginx --follow").as_deref(), Some("journalctl -u nginx --follow"));
        assert_eq!(again("docker compose logs -f web").as_deref(), Some("docker compose logs -f web"));
        assert_eq!(again("kubectl logs -f pod/x").as_deref(), Some("kubectl logs -f pod/x"));
        assert_eq!(again("tail -n 50 log.txt"), None);
        assert_eq!(again("kubectl logs pod/x"), None);
    }

    #[test]
    fn session_managers_reattach() {
        assert_eq!(again("tmux").as_deref(), Some("tmux attach"));
        assert_eq!(again("tmux a -t work").as_deref(), Some("tmux a -t work"));
        assert_eq!(again("tmux new -s work").as_deref(), Some("tmux new -s work -A"));
        assert_eq!(again("tmux kill-server"), None);
        assert_eq!(again("zellij -s work").as_deref(), Some("zellij attach work"));
        assert_eq!(again("zellij").as_deref(), Some("zellij attach"));
        assert_eq!(again("zellij attach work").as_deref(), Some("zellij attach work"));
        assert_eq!(again("screen").as_deref(), Some("screen -r"));
        assert_eq!(again("screen -x main").as_deref(), Some("screen -x main"));
        assert_eq!(again("screen make"), None);
    }

    #[test]
    fn remote_shells_reconnect_but_remote_commands_dont_rerun() {
        assert_eq!(again("ssh orin").as_deref(), Some("ssh orin"));
        assert_eq!(again("ssh -p 2222 -i ~/.ssh/k -A me@host").as_deref(), Some("ssh -p 2222 -i ~/.ssh/k -A me@host"));
        assert_eq!(again("ssh host rm -rf /tmp/x"), None);
        assert_eq!(again("ssh -t host htop"), None);
        assert_eq!(again("mosh -p 60001 host").as_deref(), Some("mosh -p 60001 host"));
        assert_eq!(again("mosh host -- tmux a"), None);
    }

    #[test]
    fn dev_servers_have_their_own_kind() {
        for command in [
            "npm run dev",
            "pnpm dev",
            "yarn start",
            "bun run preview -- --port 4000",
            "npx next dev -p 3001",
            "vite",
            "cargo watch -x run",
            "docker compose -f dev.yml up",
            "python3 manage.py runserver 8080",
            "python -m http.server 8000",
            "rails s",
            "deno task dev",
            "uvicorn app:main --reload",
        ] {
            assert_eq!(kind(command), Some(Kind::Server), "{command}");
        }
        for command in
            ["npm install", "npm run deploy", "cargo build", "docker compose down", "python script.py", "next build"]
        {
            assert_eq!(kind(command), None, "{command}");
        }
    }

    #[test]
    fn nothing_runs_again_through_sudo_pipes_or_lists() {
        for command in [
            "sudo htop",
            "doas nvtop",
            "htop | tee log",
            "vim a && make",
            "tail -f $LOG",
            "npm run dev > out.log",
            "rm -rf build",
            "make deploy",
            "git push",
        ] {
            assert_eq!(line(command), None, "{command}");
        }
    }
}
