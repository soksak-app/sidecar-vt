//! 셸 통합: zsh 와 bash 가 OSC 133 표시를 보내도록 사용자의 파일을 바꾸지 않고 시작 환경을 정한다.
//! 스크립트는 실행 파일에 포함되고, 세션을 시작하기 전에 내용 해시로 이름 붙인 임시 디렉터리에 기록된다.

use portable_pty::CommandBuilder;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const ZSH_ENV: &str = include_str!("shell-integration/zshenv");
const BASH_INTEGRATION: &str = include_str!("shell-integration/bash-integration.bash");

/// bash 가 첫 프롬프트에서 실행할 PROMPT_COMMAND 항목. 스크립트가 같은 문자열을 찾아 지운다.
/// source 로 읽는 파일 안에서는 DEBUG 트랩이 보이지 않으므로 먼저 사용자의 트랩을 기록한다. 명령 치환에서
/// 부모의 트랩을 보여 주는 것은 치환의 명령이 trap 일 때뿐이라 builtin 을 붙이지 않는다.
const BASH_ENTRY: &str =
    "_soksak_debug_trap=$(trap -p DEBUG); builtin source \"$SOKSAK_BASH_INTEGRATION\"";

/// 스크립트 내용의 FNV-1a 64 해시. 실행 파일이 바뀌어 내용이 달라지면 다른 디렉터리를 쓴다.
fn content_hash() -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in ZSH_ENV.bytes().chain([0]).chain(BASH_INTEGRATION.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 디렉터리가 이 계정 소유의 0700 디렉터리인지 확인한다. 다른 소유자나 심볼릭 링크는 쓰지 않는다.
fn check_private_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("shell integration directory {}: {error}", path.display()))?;
    if !metadata.is_dir() {
        return Err(format!(
            "shell integration path {} is not a directory",
            path.display()
        ));
    }
    if metadata.uid() != unsafe { libc::getuid() } {
        return Err(format!(
            "shell integration directory {} has another owner",
            path.display()
        ));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "shell integration directory {} is accessible to other users",
            path.display()
        ));
    }
    Ok(())
}

fn private_directory(path: &Path) -> Result<(), String> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "create shell integration directory {}: {error}",
                path.display()
            ))
        }
    }
    check_private_directory(path)
}

/// 내용이 같은 파일은 그대로 두고, 다르거나 없으면 임시 파일에 쓴 뒤 이름을 바꿔 한 번에 바꾼다.
fn write_file(path: &Path, content: &str) -> Result<(), String> {
    if fs::read(path).ok().as_deref() == Some(content.as_bytes()) {
        return Ok(());
    }
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| {
            format!(
                "write shell integration file {}: {error}",
                temporary.display()
            )
        })?;
    file.write_all(content.as_bytes()).map_err(|error| {
        format!(
            "write shell integration file {}: {error}",
            temporary.display()
        )
    })?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("install shell integration file {}: {error}", path.display()))
}

/// 통합 디렉터리를 준비하고 그 경로를 돌려준다.
pub fn install() -> Result<PathBuf, String> {
    let root = std::env::temp_dir().join(format!("soksak-shell-{:016x}", content_hash()));
    private_directory(&root)?;
    let zsh = root.join("zsh");
    private_directory(&zsh)?;
    write_file(&zsh.join(".zshenv"), ZSH_ENV)?;
    write_file(&root.join("bash-integration.bash"), BASH_INTEGRATION)?;
    Ok(root)
}

/// 셸 이름에 맞는 통합 환경을 command 에 더한다. zsh 와 bash 가 아닌 셸은 바꾸지 않는다.
pub fn apply(shell: &str, command: &mut CommandBuilder) -> Result<(), String> {
    let name = Path::new(shell)
        .file_name()
        .and_then(|name| name.to_str())
        // 기본값: 이름을 읽지 못한 셸은 zsh 나 bash 가 아니므로 바꾸지 않는다.
        .unwrap_or_default();
    match name {
        "zsh" => {
            let root = install()?;
            if let Some(original) = command.get_env("ZDOTDIR").map(|value| value.to_owned()) {
                command.env("SOKSAK_ZSH_ZDOTDIR", original);
            }
            command.env("ZDOTDIR", root.join("zsh"));
        }
        "bash" => {
            let root = install()?;
            command.env(
                "SOKSAK_BASH_INTEGRATION",
                root.join("bash-integration.bash"),
            );
            let inherited = match command.get_env("PROMPT_COMMAND") {
                None => String::new(),
                Some(value) => value
                    .to_str()
                    .ok_or("the inherited PROMPT_COMMAND is not UTF-8")?
                    .to_string(),
            };
            // 항목 뒤에 구분자를 두지 않는다. 시작 파일이 "$PROMPT_COMMAND; x" 로 덧붙여도 문법이 맞는다.
            let value = if inherited.is_empty() {
                BASH_ENTRY.to_string()
            } else {
                format!("{BASH_ENTRY}\n{inherited}")
            };
            command.env("PROMPT_COMMAND", value);
        }
        _ => {}
    }
    Ok(())
}
