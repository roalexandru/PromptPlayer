//! §16 — standalone CLI over the typing engine. Reads stdin and types into the
//! focused window with the full cadence model; `--dry-run` prints the schedule
//! as JSON instead. See `--help` for the flags.

use prompt_player::inject::EnigoInjector;
use prompt_player::typer::{
    play, schedule, Profile, ProfileKind, RecordingInjector, ScheduleOptions,
};
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::io::Read;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Debug)]
struct Args {
    profile_kind: ProfileKind,
    rdp: bool,
    seed: u64,
    dry_run: bool,
    typo_rate_override: Option<f64>,
    iki_median_override_ms: Option<f64>,
    no_pre_typing_pause: bool,
    final_enter: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            profile_kind: ProfileKind::SalesEngineer,
            rdp: false,
            seed: 42,
            dry_run: false,
            typo_rate_override: None,
            iki_median_override_ms: None,
            no_pre_typing_pause: false,
            final_enter: false,
        }
    }
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < raw.len() {
        let a = &raw[i];
        match a.as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--profile" => {
                i += 1;
                let v = raw.get(i).ok_or("--profile requires a value")?;
                args.profile_kind = match v.as_str() {
                    "sales-engineer" | "se" => ProfileKind::SalesEngineer,
                    "fast-presenter" | "fast" => ProfileKind::FastPresenter,
                    "thoughtful-ceo" | "ceo" => ProfileKind::ThoughtfulCeo,
                    other => return Err(format!("unknown profile: {}", other)),
                };
            }
            "--rdp" => args.rdp = true,
            "--seed" => {
                i += 1;
                args.seed = raw
                    .get(i)
                    .ok_or("--seed requires a value")?
                    .parse()
                    .map_err(|e| format!("--seed: {}", e))?;
            }
            "--dry-run" => args.dry_run = true,
            "--typo-rate" => {
                i += 1;
                args.typo_rate_override = Some(
                    raw.get(i)
                        .ok_or("--typo-rate requires a value")?
                        .parse()
                        .map_err(|e| format!("--typo-rate: {}", e))?,
                );
            }
            "--iki-median-ms" => {
                i += 1;
                args.iki_median_override_ms = Some(
                    raw.get(i)
                        .ok_or("--iki-median-ms requires a value")?
                        .parse()
                        .map_err(|e| format!("--iki-median-ms: {}", e))?,
                );
            }
            "--no-pre-typing-pause" => args.no_pre_typing_pause = true,
            "--final-enter" => args.final_enter = true,
            other => return Err(format!("unknown arg: {}", other)),
        }
        i += 1;
    }
    Ok(args)
}

fn print_help() {
    println!(
        r#"typing-engine-cli — Prompt Player Phase 1 prototype

Reads text from stdin and types it into the focused window with
human cadence (log-normal mixture, hierarchical pauses, typo + correction).

Options:
  --profile <name>       sales-engineer (default), fast-presenter, thoughtful-ceo
  --rdp                  RDP-mode timing adjustments (§9.3)
  --seed <u64>           Deterministic RNG seed (default 42)
  --dry-run              Print JSON schedule instead of typing
  --typo-rate <f64>      Override profile typo rate (e.g. 0.0 to disable)
  --iki-median-ms <f64>  Override IKI median in ms
  --no-pre-typing-pause  Skip the §3.1 pre-typing pause
  --final-enter          Press Enter after the last character
  --help, -h             This help
"#
    );
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {}", e);
            print_help();
            std::process::exit(2);
        }
    };

    let mut text = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut text) {
        eprintln!("error reading stdin: {}", e);
        std::process::exit(1);
    }
    let text = text.trim_end_matches('\n').to_string();

    // Build profile with overrides.
    let mut profile = Profile::from_kind(args.profile_kind);
    profile.send_final_enter = args.final_enter;
    if let Some(rate) = args.typo_rate_override {
        profile.typo_rate = rate;
        profile.typos_enabled = rate > 0.0;
    }
    if let Some(median) = args.iki_median_override_ms {
        profile.iki_scale = median / 140.0;
    }

    let opts = ScheduleOptions {
        rdp_mode: args.rdp,
        include_pre_typing_pause: !args.no_pre_typing_pause,
    };

    let mut rng = ChaCha8Rng::seed_from_u64(args.seed);
    let sched = schedule(&text, &profile, &opts, &mut rng);

    if args.dry_run {
        match serde_json::to_string_pretty(&sched) {
            Ok(s) => println!("{}", s),
            Err(e) => {
                eprintln!("serialize error: {}", e);
                std::process::exit(1);
            }
        }
        return;
    }

    eprintln!(
        "scheduled {} keys, total {} ms — typing in 3s, focus the target window now",
        sched.len(),
        sched.last().map(|k| k.absolute_time_ms).unwrap_or(0),
    );
    std::thread::sleep(std::time::Duration::from_secs(3));

    match EnigoInjector::new() {
        Ok(mut inj) => {
            let cancel = Arc::new(AtomicBool::new(false));
            let completed = play(&sched, &mut inj, cancel);
            if !completed {
                eprintln!("playback cancelled");
                std::process::exit(130);
            }
        }
        Err(e) => {
            eprintln!("Failed to init enigo: {}", e);
            eprintln!(
                "On macOS, grant Accessibility permission to the terminal you're running this from."
            );
            // Fall back to recording so the rest of the harness still works in CI.
            let mut inj = RecordingInjector::default();
            let cancel = Arc::new(AtomicBool::new(false));
            let _ = play(&sched, &mut inj, cancel);
            std::process::exit(1);
        }
    }
}
