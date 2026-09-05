//! Capture control. docs/cli.md: "Pause state must be visible."
//!
//! Pause is a file rather than a settings row so the hook can answer "am I
//! paused?" without opening SQLite, which is what keeps it inside the 5ms
//! budget.

use crate::capture::now_ms;
use crate::paths;
use anyhow::Result;

pub enum Pause {
    No,
    Indefinite,
    Until(i64),
}

pub fn state() -> Result<Pause> {
    let p = paths::pause_file()?;
    let Ok(text) = std::fs::read_to_string(&p) else {
        return Ok(Pause::No);
    };
    let t = text.trim();
    if t.is_empty() || t == "forever" {
        return Ok(Pause::Indefinite);
    }
    match t.parse::<i64>() {
        Ok(until) if until > now_ms() => Ok(Pause::Until(until)),
        Ok(_) => {
            let _ = std::fs::remove_file(&p); // auto-resume
            Ok(Pause::No)
        }
        Err(_) => Ok(Pause::Indefinite),
    }
}

/// The check the hook makes, cheaply. `TMEM=0` is the per-invocation escape.
pub fn capture_enabled() -> bool {
    if std::env::var("TMEM").map(|v| v == "0").unwrap_or(false) {
        return false;
    }
    !matches!(state(), Ok(Pause::Indefinite) | Ok(Pause::Until(_)))
}

pub fn pause(duration: Option<&str>) -> Result<i32> {
    let path = paths::pause_file()?;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    match duration {
        None => {
            std::fs::write(&path, "forever")?;
            println!("capture paused — nothing will be recorded until `tmem resume`");
        }
        Some(d) => {
            let ms = parse_duration(d)?;
            let until = now_ms() + ms;
            std::fs::write(&path, until.to_string())?;
            println!(
                "capture paused for {d} — resumes automatically at {}",
                crate::output::fmt_datetime(until)
            );
        }
    }
    Ok(crate::output::EXIT_OK)
}

pub fn resume() -> Result<i32> {
    let path = paths::pause_file()?;
    if path.exists() {
        std::fs::remove_file(&path)?;
        println!("capture resumed");
    } else {
        println!("capture was not paused");
    }
    Ok(crate::output::EXIT_OK)
}

fn parse_duration(s: &str) -> Result<i64> {
    let s = s.trim();
    let idx = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let n: i64 = s[..idx]
        .parse()
        .map_err(|_| anyhow::anyhow!("cannot understand duration '{s}'; try `2h` or `30m`"))?;
    let unit = s[idx..].trim();
    let ms = match unit.trim_end_matches('s') {
        "m" | "min" | "minute" => 60_000,
        "h" | "hr" | "hour" | "" => 3_600_000,
        "d" | "day" => 86_400_000,
        other => anyhow::bail!("unknown duration unit '{other}'; try `2h` or `30m`"),
    };
    Ok(n * ms)
}
