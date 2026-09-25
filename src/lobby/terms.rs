// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// Personal mode logs in to the lobby as a player, and the stock client has a
// player accept the lobby's terms before it logs in. The server asks the same
// once, at startup, and records the answer in its config file.

use std::io::IsTerminal;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use toml_edit::DocumentMut;

// The copies the 0.28.0 game ships and shows at its lobby login, as plain
// text rather than the web page the game links to.
const TERMS_BASE_URL: &str = "https://gitea.wildfiregames.com/0ad/0ad/raw/tag/v0.28.0/binaries/data/mods/public/gui/prelobby/common/terms/";
const DOCUMENTS: [(&str, &str); 3] = [
    ("Terms of Service", "Terms_of_Service.txt"),
    ("Terms of Use", "Terms_of_Use.txt"),
    ("Privacy Policy", "Privacy_Policy.txt"),
];
// Startup must not hang on a server that stopped answering.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
const ACCEPTANCE_KEY: &str = "i_accept_terms_of_service_and_terms_of_use_and_privacy_policy";
const ACCEPT_ANSWER: &str = "YES";

// Ok when the terms are accepted, already or now. Without a terminal to ask
// on, the operator is told to accept them in the config file instead.
pub async fn ensure_accepted(accepted: bool, config_path: Option<&Path>) -> Result<(), String> {
    if accepted {
        return Ok(());
    }
    let config_name = config_path.map_or_else(
        || "your config file".to_string(),
        |path| format!("'{}'", path.display()),
    );
    if !std::io::stdin().is_terminal() {
        return Err(format!(
            "personal mode logs in to the lobby as you, which needs its terms accepted. \
             Read {} and then set [personal] {ACCEPTANCE_KEY} = true in {config_name}",
            urls().join(", ")
        ));
    }

    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|error| format!("cannot set up downloading the lobby's terms: {error}"))?;
    let mut texts = Vec::with_capacity(DOCUMENTS.len());
    for ((title, _), url) in DOCUMENTS.iter().zip(urls()) {
        let text = download(&client, &url).await?;
        texts.push((*title, url, text));
    }

    {
        let mut out = std::io::stdout().lock();
        for (title, url, text) in &texts {
            let _ = writeln!(
                out,
                "\n===== {title} =====\n{url}\n\n{}",
                strip_markup(text)
            );
        }
        let _ = write!(
            out,
            "\nThe server logs in to the lobby as you. Type {ACCEPT_ANSWER} to accept the \
             Terms of Service, the Terms of Use and the Privacy Policy above: "
        );
        let _ = out.flush();
    }
    let answer = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).map(|_| line)
    })
    .await
    .map_err(|error| format!("cannot read the answer: {error}"))?
    .map_err(|error| format!("cannot read the answer: {error}"))?;
    if answer.trim() != ACCEPT_ANSWER {
        return Err(
            "the lobby's terms were not accepted, so personal mode cannot log in".to_string(),
        );
    }

    match config_path {
        Some(path) => record_acceptance(path)?,
        None => println!("Accepted for this run only: there is no config file to record it in."),
    }
    Ok(())
}

fn urls() -> Vec<String> {
    DOCUMENTS
        .iter()
        .map(|(_, file)| format!("{TERMS_BASE_URL}{file}"))
        .collect()
}

async fn download(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| format!("cannot download {url}: {error}"))?;
    response
        .text()
        .await
        .map_err(|error| format!("cannot download {url}: {error}"))
}

// The game's text markup; the terms use only font changes, so any other
// bracket is left as written.
fn strip_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        match tail.find(']') {
            Some(end) if is_font_tag(&tail[..=end]) => rest = &tail[end + 1..],
            _ => {
                out.push('[');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn is_font_tag(tag: &str) -> bool {
    tag.starts_with("[font=") || tag == "[/font]"
}

// Edited in place so the file keeps its comments and layout, and written
// aside then renamed so a crash cannot leave it half written. The file holds
// the lobby password, so the copy keeps the original's permissions.
fn record_acceptance(path: &Path) -> Result<(), String> {
    let shown = path.display();
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read '{shown}' to record the acceptance: {error}"))?;
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|error| format!("cannot parse '{shown}' to record the acceptance: {error}"))?;
    doc["personal"][ACCEPTANCE_KEY] = toml_edit::value(true);
    let permissions = std::fs::metadata(path)
        .map_err(|error| format!("cannot read '{shown}': {error}"))?
        .permissions();
    let temp = path.with_extension("toml.tmp");
    std::fs::write(&temp, doc.to_string())
        .and_then(|()| std::fs::set_permissions(&temp, permissions))
        .and_then(|()| std::fs::rename(&temp, path))
        .map_err(|error| format!("cannot record the acceptance in '{shown}': {error}"))?;
    println!("Recorded in '{shown}'.");
    Ok(())
}
