//! `blog-release`: the maintainers' signing tool (docs/ARCHITECTURE.md, Release). Not shipped.
//!
//! - `keygen <dir> [--out <file>]`: writes `<dir>/minisign.pub`; prints the secret once (for
//!   the GitHub secret `PLUMBLOG_RELEASE_KEY`), or writes it to a new 0600 file with `--out`.
//! - `sign <file>`: key from `PLUMBLOG_RELEASE_KEY`; writes `<file>.minisig`.
//! - `verify <file> <pub>`: checks `<file>.minisig` against a public key file.

use plumblog_cli::release::minisign::{KeyError, PublicKey, SecretKey, key_id_hex};
use plumblog_cli::text::{Code, Text, pick_lang};
use plumblog_i18n::{Key, key};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const SECRET_ENV: &str = "PLUMBLOG_RELEASE_KEY";

type Fail = (Code, String);

fn t(text: &Text, k: Key, args: &[(&str, String)]) -> String {
    text.t(k, args)
}

fn file_failed(text: &Text, path: &Path, e: &std::io::Error) -> Fail {
    (
        Code::ReleaseFileFailed,
        t(
            text,
            key!("cli.release_tool.file_failed"),
            &[
                ("path", path.display().to_string()),
                ("reason", e.to_string()),
            ],
        ),
    )
}

fn usage(text: &Text) -> Fail {
    (
        Code::ArgsInvalid,
        t(text, key!("cli.release_tool.usage"), &[]),
    )
}

/// A new file only, owner-only on Unix: the secret must not be readable by anyone else.
fn write_private(path: &Path, data: &str) -> std::io::Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(data.as_bytes())?;
    f.sync_all()
}

fn keygen(text: &Text, dir: &Path, out: Option<&Path>) -> Result<String, Fail> {
    let pub_path = dir.join("minisign.pub");
    // Replacing a real key would orphan every release signed with it: only the placeholder.
    if let Ok(old) = fs::read_to_string(&pub_path)
        && PublicKey::parse(&old) != Err(KeyError::Placeholder)
    {
        return Err((
            Code::ReleaseKeyExists,
            t(
                text,
                key!("cli.release_tool.key_exists"),
                &[("path", pub_path.display().to_string())],
            ),
        ));
    }
    let key = SecretKey::generate().map_err(|e| {
        let msg = t(text, key!("cli.error.internal"), &[("detail", e)]);
        (Code::Internal, msg)
    })?;
    // The secret goes out first: a public key without its secret would be useless.
    let secret_line = match out {
        Some(p) => {
            write_private(p, &format!("{}\n", key.to_secret()))
                .map_err(|e| file_failed(text, p, &e))?;
            t(
                text,
                key!("cli.release_tool.secret_written"),
                &[
                    ("path", p.display().to_string()),
                    ("var", SECRET_ENV.into()),
                ],
            )
        }
        None => t(
            text,
            key!("cli.release_tool.secret_once"),
            &[("var", SECRET_ENV.into()), ("secret", key.to_secret())],
        ),
    };
    fs::write(&pub_path, key.public().to_file()).map_err(|e| file_failed(text, &pub_path, &e))?;
    let done = t(
        text,
        key!("cli.release_tool.keygen_done"),
        &[
            ("path", pub_path.display().to_string()),
            ("key_id", key_id_hex(&key.public().key_id)),
        ],
    );
    Ok(format!("{done}\n{secret_line}\n"))
}

fn sign(text: &Text, file: &Path) -> Result<String, Fail> {
    let secret = std::env::var(SECRET_ENV).unwrap_or_default();
    if secret.trim().is_empty() {
        let msg = t(
            text,
            key!("cli.release_tool.secret_missing"),
            &[("var", SECRET_ENV.into())],
        );
        return Err((Code::ReleaseSecretMissing, msg));
    }
    let key = SecretKey::parse(secret.trim()).map_err(|why| {
        let msg = t(
            text,
            key!("cli.release_tool.secret_invalid"),
            &[("var", SECRET_ENV.into()), ("reason", why.into())],
        );
        (Code::ReleaseSecretInvalid, msg)
    })?;
    let data = fs::read(file).map_err(|e| file_failed(text, file, &e))?;
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    // minisign's own trusted comment shape.
    let sig = key.sign(&data, &format!("timestamp:{now}\tfile:{name}"));
    let mut out = file.as_os_str().to_owned();
    out.push(".minisig");
    let out = PathBuf::from(out);
    fs::write(&out, sig).map_err(|e| file_failed(text, &out, &e))?;
    Ok(format!(
        "{}\n",
        t(
            text,
            key!("cli.release_tool.signed"),
            &[
                ("path", out.display().to_string()),
                ("key_id", key_id_hex(&key.public().key_id)),
            ],
        )
    ))
}

fn verify(text: &Text, file: &Path, pub_file: &Path) -> Result<String, Fail> {
    let pk_text = fs::read_to_string(pub_file).map_err(|e| file_failed(text, pub_file, &e))?;
    let invalid = |reason: String| {
        let msg = t(
            text,
            key!("cli.release_tool.verify_failed"),
            &[("reason", reason)],
        );
        (Code::UpdateSignatureInvalid, msg)
    };
    let pk = PublicKey::parse(&pk_text).map_err(|e| invalid(format!("{e:?}")))?;
    let mut sig_path = file.as_os_str().to_owned();
    sig_path.push(".minisig");
    let sig_path = PathBuf::from(sig_path);
    let sig = fs::read_to_string(&sig_path).map_err(|e| file_failed(text, &sig_path, &e))?;
    let data = fs::read(file).map_err(|e| file_failed(text, file, &e))?;
    let comment = pk
        .verify(&sig, &data)
        .map_err(|e| invalid(format!("{e:?}")))?;
    Ok(format!(
        "{}\n",
        t(
            text,
            key!("cli.release_tool.verified"),
            &[(
                "comment",
                comment
                    .chars()
                    .filter(|c| !c.is_control() || *c == '\t')
                    .collect()
            )],
        )
    ))
}

fn run(text: &Text, args: &[String]) -> Result<String, Fail> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["keygen", dir] => keygen(text, Path::new(dir), None),
        ["keygen", dir, "--out", file] => keygen(text, Path::new(dir), Some(Path::new(file))),
        ["sign", file] => sign(text, Path::new(file)),
        ["verify", file, pub_file] => verify(text, Path::new(file), Path::new(pub_file)),
        _ => Err(usage(text)),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let text = Text::new(&pick_lang(None, &|k| std::env::var(k).ok()));
    match run(&text, &args) {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err((code, msg)) => {
            eprintln!("{}", text.error_line(code, &msg));
            ExitCode::FAILURE
        }
    }
}
