//! What kind of work a shell command is (PLAN.md §8 item 6). The hook runs
//! this locally and forwards only the class, never the command. Simple
//! prefix and keyword matching: being wrong just shows the plain terminal.

/// The class of the first part of a command line that has one, so
/// `cd app && cargo test 2>&1 | tail` is a test.
pub fn classify(command: &str) -> Option<&'static str> {
    command.split(['\n', ';', '|', '&']).find_map(classify_one)
}

fn program(word: &str) -> String {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word).to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat"] {
        if let Some(stem) = name.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    name
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
}

fn classify_one(segment: &str) -> Option<&'static str> {
    const TEST: Option<&str> = Some("test");
    const BUILD: Option<&str> = Some("build");
    const INSTALL: Option<&str> = Some("install");
    let mut words = segment.split_whitespace().map(|w| w.trim_matches(['"', '\'']));
    let mut prog = loop {
        let word = words.next()?;
        if is_assignment(word) {
            continue;
        }
        let prog = program(word);
        if !matches!(prog.as_str(), "sudo" | "time" | "env" | "npx" | "pnpx" | "bunx" | "command") {
            break prog;
        }
    };
    if matches!(prog.as_str(), "uv" | "poetry" | "pipenv") {
        let mut next = words.clone();
        if next.next() == Some("run") {
            prog = program(next.next()?);
            words = next;
        }
    }
    let rest: Vec<&str> = words.take(6).collect();
    let args = || rest.iter().copied().filter(|w| !w.starts_with(['-', '+']));
    let sub = args().next().unwrap_or("");
    let script = if sub == "run" { args().nth(1).unwrap_or("") } else { sub };
    let installs = matches!(sub, "install" | "add" | "i");
    match prog.as_str() {
        "git" | "gh" => Some("git"),
        "pytest" | "jest" | "vitest" | "mocha" | "rspec" | "ctest" | "phpunit" => TEST,
        "tsc" | "webpack" | "rollup" | "esbuild" | "msbuild" | "ninja" => BUILD,
        "pip" | "pip3" | "apt" | "apt-get" | "brew" | "winget" | "choco" | "scoop" | "pacman" => installs.then_some("install"),
        "make" | "cmake" | "gradle" | "gradlew" => if sub == "test" { TEST } else { BUILD },
        "cargo" => match sub {
            "test" | "nextest" => TEST,
            "build" | "check" | "clippy" => BUILD,
            "install" | "add" | "update" => INSTALL,
            _ => None,
        },
        "go" | "dotnet" | "swift" | "zig" => match sub {
            "test" => TEST,
            "build" => BUILD,
            _ => None,
        },
        "npm" | "pnpm" | "yarn" | "bun" => match script {
            s if s == "t" || s.starts_with("test") => TEST,
            s if s.starts_with("build") => BUILD,
            "install" | "i" | "ci" | "add" => INSTALL,
            _ => None,
        },
        "mvn" => match sub {
            "test" => TEST,
            "compile" | "package" | "install" | "verify" => BUILD,
            _ => None,
        },
        "python" | "python3" | "py" if rest.first() == Some(&"-m") => match rest.get(1).copied() {
            Some("pytest" | "unittest") => TEST,
            Some("pip") if rest.get(2) == Some(&"install") => INSTALL,
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::classify;

    fn check(class: Option<&str>, commands: &[&str]) {
        for c in commands {
            assert_eq!(classify(c), class, "{c}");
        }
    }

    #[test]
    fn git() {
        check(Some("git"), &["git status", "git -C foo log", "gh pr view 3", r"C:\Tools\git.exe diff"]);
    }

    #[test]
    fn tests() {
        check(Some("test"), &["cargo test", "cargo +nightly test --all", "npm test", "npm run test:unit", "pnpm run test", "yarn t", "pytest -x", "python -m pytest", "uv run pytest", "npx jest", "go test ./...", "dotnet test", "make test", "mvn test"]);
    }

    #[test]
    fn builds() {
        check(Some("build"), &["cargo build --release", "cargo clippy --all-targets", "npm run build", "make", "cmake --build .", "go build ./cmd/x", "tsc -p .", "mvn package", "dotnet build"]);
    }

    #[test]
    fn installs() {
        check(Some("install"), &["npm install", "npm ci", "pnpm add left-pad", "cargo add serde", "pip install requests", "python -m pip install x", "winget install Foo", "sudo apt-get install jq"]);
    }

    #[test]
    fn everything_else_is_plain() {
        check(None, &["", "ls -la", "cd src", "echo hello", "cargo run", "npm start", "python main.py", "pip list", "docker ps", "rm -rf target"]);
    }

    #[test]
    fn the_real_command_is_found_in_a_longer_line() {
        check(Some("test"), &["cd app && cargo test 2>&1 | tail -3", "FOO=1 BAR=2 cargo test", "Set-Location x; cargo test", "& cargo test", "cd a\ncargo test"]);
        assert_eq!(classify("cargo build && cargo test"), Some("build"), "the first one wins");
    }
}
