//! `dmind-eval`: budgeted offline/live evaluation of the configured dMind
//! provider against the synthetic corpus.
//!
//! Secrets are never accepted as arguments; provider identity, endpoint and
//! key come from the same environment variables as the server
//! (`DMIND_MODEL_PROVIDER`, `DMIND_MODEL_ENDPOINT`, `DMIND_MODEL_API_KEY`…).
//! Live mode additionally requires `WELLOS_ALLOW_EXTERNAL_AI=true`,
//! `WELLOS_EVAL_LIVE=true`, `WELLOS_ENV` test|staging, a synthetic tenant and
//! `--confirm-live`. Nothing is written outside the `dmind_eval_*` tables.

use std::process::ExitCode;

use dmind_gateway::Operation;
use rust_decimal::Decimal;
use uuid::Uuid;
use wellos_server::eval::{self, report, runner, Budget, Corpus, LiveGate, Mode, Pricing, Runner};
use wellos_server::runtime::{model_gateway_from_env, RuntimeConfig, RuntimeEnv};

const USAGE: &str = "\
usage: dmind-eval [options]
  --suite <key>                 suite key (default dmind-core)
  --suite-version <semver>      corpus version (default compiled-in)
  --operations <a,b,...>        operation keys; default: full registry
  --mode dry-run|offline|live   default dry-run
  --provider env|fake           env = configured DMIND_MODEL_PROVIDER; fake = synthetic fixture (offline only)
  --tenant-id <uuid>            synthetic tenant (required for --persist and live)
  --confirm-live                explicit acknowledgement for live mode
  --max-calls <n>               default 120 (covers the full synthetic corpus offline; live runs should pass an explicit ceiling)
  --max-concurrency <n>         default 2 (1..16)
  --max-input-tokens <n>        default 400000
  --max-output-tokens <n>       default 200000
  --max-output-per-call <n>     worst-case output reserved per in-flight call (default 4000)
  --no-cache                    do not reuse prior successful executions across runs
  --max-cost <decimal>          maximum estimated spend (required for live)
  --currency <ISO>              pricing currency (default USD)
  --price-in <per 1k tokens>    input price (required for live)
  --price-out <per 1k tokens>   output price (required for live)
  --price-date <YYYY-MM-DD>     pricing effective date (required for live)
  --no-stop-on-high-severity    keep running after a high-severity safety failure
  --json <path>                 write the JSON report
  --html <path>                 write the self-contained HTML report
  --persist                     write the run to the dmind_eval_* tables (DATABASE_URL)
  --list-operations             print the registry and exit
exit codes: 0 ok · 1 error · 2 safety/quality failure or abort · 3 gate or plan refused";

struct Args {
    suite: String,
    suite_version: Option<String>,
    operations: Option<Vec<Operation>>,
    mode: Mode,
    provider: String,
    tenant_id: Option<Uuid>,
    confirm_live: bool,
    budget: Budget,
    stop_on_high_severity: bool,
    json: Option<String>,
    html: Option<String>,
    persist: bool,
    list: bool,
    reuse_cache: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        suite: eval::SUITE_KEY.to_string(),
        suite_version: None,
        operations: None,
        mode: Mode::DryRun,
        provider: "env".into(),
        tenant_id: None,
        confirm_live: false,
        budget: Budget {
            max_calls: 120,
            max_concurrency: 2,
            max_input_tokens: 400_000,
            max_output_tokens: 200_000,
            max_estimated_cost: None,
            pricing: None,
            max_output_tokens_per_call: 2_000,
        },
        stop_on_high_severity: true,
        json: None,
        html: None,
        persist: false,
        list: false,
        reuse_cache: true,
    };
    let mut currency = "USD".to_string();
    let mut price_in: Option<Decimal> = None;
    let mut price_out: Option<Decimal> = None;
    let mut price_date: Option<chrono::NaiveDate> = None;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = |name: &str| it.next().ok_or(format!("{name} requires a value"));
        match flag.as_str() {
            "--suite" => a.suite = val("--suite")?,
            "--suite-version" => a.suite_version = Some(val("--suite-version")?),
            "--operations" => {
                let raw = val("--operations")?;
                let mut ops = Vec::new();
                for key in raw.split(',').map(str::trim).filter(|k| !k.is_empty()) {
                    ops.push(Operation::parse(key).ok_or(format!("unknown operation {key}"))?);
                }
                a.operations = Some(ops);
            }
            "--mode" => {
                let raw = val("--mode")?;
                a.mode = Mode::parse(&raw).ok_or(format!("unknown mode {raw}"))?;
            }
            "--provider" => a.provider = val("--provider")?,
            "--tenant-id" => {
                a.tenant_id =
                    Some(Uuid::parse_str(&val("--tenant-id")?).map_err(|e| e.to_string())?)
            }
            "--confirm-live" => a.confirm_live = true,
            "--max-calls" => a.budget.max_calls = num(&val("--max-calls")?)?,
            "--max-concurrency" => a.budget.max_concurrency = num(&val("--max-concurrency")?)?,
            "--max-input-tokens" => a.budget.max_input_tokens = num(&val("--max-input-tokens")?)?,
            "--max-output-per-call" => {
                a.budget.max_output_tokens_per_call = num(&val("--max-output-per-call")?)?
            }
            "--no-cache" => a.reuse_cache = false,
            "--max-output-tokens" => {
                a.budget.max_output_tokens = num(&val("--max-output-tokens")?)?
            }
            "--max-cost" => a.budget.max_estimated_cost = Some(dec(&val("--max-cost")?)?),
            "--currency" => currency = val("--currency")?,
            "--price-in" => price_in = Some(dec(&val("--price-in")?)?),
            "--price-out" => price_out = Some(dec(&val("--price-out")?)?),
            "--price-date" => {
                price_date = Some(
                    chrono::NaiveDate::parse_from_str(&val("--price-date")?, "%Y-%m-%d")
                        .map_err(|e| e.to_string())?,
                )
            }
            "--no-stop-on-high-severity" => a.stop_on_high_severity = false,
            "--json" => a.json = Some(val("--json")?),
            "--html" => a.html = Some(val("--html")?),
            "--persist" => a.persist = true,
            "--list-operations" => a.list = true,
            "-h" | "--help" => return Err(USAGE.to_string()),
            other => {
                if other.contains("key") || other.contains("secret") || other.contains("token=") {
                    return Err(
                        "secrets are never accepted on the command line; use the environment"
                            .into(),
                    );
                }
                return Err(format!("unknown argument {other}\n{USAGE}"));
            }
        }
    }
    if price_in.is_some() || price_out.is_some() || price_date.is_some() {
        a.budget.pricing = Some(Pricing {
            currency,
            input_per_1k_tokens: price_in.ok_or("--price-in missing")?,
            output_per_1k_tokens: price_out.ok_or("--price-out missing")?,
            effective_date: price_date.ok_or("--price-date missing")?,
        });
    }
    Ok(a)
}

fn num<T: std::str::FromStr>(raw: &str) -> Result<T, String> {
    raw.parse().map_err(|_| format!("invalid number {raw}"))
}

fn dec(raw: &str) -> Result<Decimal, String> {
    raw.parse().map_err(|_| format!("invalid decimal {raw}"))
}

fn env_true(name: &str) -> bool {
    std::env::var(name)
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    match run().await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("dmind-eval: {e}");
            ExitCode::from(1)
        }
    }
}

async fn run() -> Result<ExitCode, String> {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return Ok(ExitCode::from(if e == USAGE { 0 } else { 3 }));
        }
    };
    let corpus = Corpus::load()?;
    corpus.check_coverage()?;
    if args.list {
        for op in eval::registry() {
            println!(
                "{:<30} {:<40} baseline={}",
                op.key, op.output_schema, op.deterministic_baseline
            );
        }
        return Ok(ExitCode::SUCCESS);
    }
    if args.suite != corpus.suite_key {
        return Err(format!(
            "unknown suite {}; compiled-in suite is {}",
            args.suite, corpus.suite_key
        ));
    }
    if let Some(v) = &args.suite_version {
        if v != corpus.version {
            return Err(format!(
                "corpus version {v} not available; compiled-in is {}",
                corpus.version
            ));
        }
    }

    let env = RuntimeEnv::from_env().map_err(|e| e.to_string())?;
    let cfg = RuntimeConfig::from_env_for(env).map_err(|e| e.to_string())?;
    let pool = if args.persist || (args.mode == Mode::Live && args.tenant_id.is_some()) {
        let url = wellos_server::database_url(env).map_err(|e| e.to_string())?;
        Some(
            wellos_server::connect_pool(&url)
                .await
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    let tenant_synthetic = match (args.tenant_id, &pool) {
        (Some(t), Some(pool)) => eval::store::require_synthetic_tenant(pool, t).await.is_ok(),
        _ => false,
    };
    let live_gate = LiveGate {
        env,
        allow_external_ai: cfg.allow_external_ai,
        eval_live_enabled: env_true("WELLOS_EVAL_LIVE"),
        tenant_synthetic,
        patients_synthetic: corpus.verify_synthetic().is_ok(),
        explicit_confirmation: args.confirm_live,
    };

    let gateway = match args.provider.as_str() {
        "env" => model_gateway_from_env(&cfg).map_err(|e| e.to_string())?,
        "fake" => fixture_gateway(env)?,
        other => return Err(format!("unknown provider selector {other}")),
    };

    let runner = Runner {
        corpus: &corpus,
        operations: args.operations.clone(),
        mode: args.mode,
        budget: args.budget.clone(),
        stop_on_high_severity: args.stop_on_high_severity,
        live_gate: Some(live_gate.clone()),
        cache: match (&pool, args.tenant_id) {
            (Some(pool), Some(t)) => {
                let fp = runner::ProviderFingerprint::of(gateway.as_ref());
                eval::store::cached_executions(pool, t, &fp.config_hash)
                    .await
                    .map_err(|e| format!("{e:?}"))?
            }
            _ => vec![],
        },
    };
    let plan = match runner.plan() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("plan refused: {e}");
            return Ok(ExitCode::from(3));
        }
    };
    println!(
        "plan: suite {}@{} rubric {} mode {} cases {} planned provider calls {} (max {}) concurrency {} tokens in/out ≤ {}/{} cost ≤ {} {}",
        plan.suite_key,
        plan.suite_version,
        plan.rubric_version,
        plan.mode.as_str(),
        plan.planned_cases,
        plan.planned_provider_calls,
        plan.budget.max_calls,
        plan.budget.max_concurrency,
        plan.budget.max_input_tokens,
        plan.budget.max_output_tokens,
        plan.budget.max_estimated_cost.map(|c| c.to_string()).unwrap_or_else(|| "n/a".into()),
        plan.budget.pricing.as_ref().map(|p| p.currency.as_str()).unwrap_or(""),
    );
    println!("SYNTHETIC DATA ONLY — not clinical validation.");
    if args.mode == Mode::Live {
        println!("live gate satisfied: external provider will be called (confirmed with --confirm-live).");
    }

    let outcome = match runner.execute(gateway).await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("run refused: {e}");
            return Ok(ExitCode::from(3));
        }
    };
    print!("{}", report::to_text(&outcome));
    if let Some(p) = &args.json {
        std::fs::write(
            p,
            serde_json::to_vec_pretty(&report::to_json(&outcome)).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        println!("json report: {p}");
    }
    if let Some(p) = &args.html {
        std::fs::write(p, report::to_html(&outcome)).map_err(|e| e.to_string())?;
        println!("html report: {p}");
    }
    if args.persist {
        let (pool, tenant) = match (&pool, args.tenant_id) {
            (Some(p), Some(t)) => (p, t),
            _ => return Err("--persist requires --tenant-id and DATABASE_URL".into()),
        };
        let gate = serde_json::json!({
            "allow_external_ai": live_gate.allow_external_ai,
            "eval_live_enabled": live_gate.eval_live_enabled,
            "env": env.as_str(),
            "tenant_synthetic": live_gate.tenant_synthetic,
            "explicit_confirmation": live_gate.explicit_confirmation,
        });
        let run_id = eval::store::persist_run(
            pool,
            tenant,
            &corpus,
            &outcome,
            env.as_str(),
            None,
            "cli",
            gate,
        )
        .await
        .map_err(|e| format!("{e:?}"))?;
        println!("persisted run {run_id}");
    }
    let bad = !outcome.safety_failures.is_empty()
        || outcome.totals.failed > 0
        || outcome.abort.is_some()
        || (args.mode == Mode::Live && outcome.totals.fallback_verified > 0);
    Ok(if bad {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    })
}

#[cfg(feature = "dev-fixtures")]
fn fixture_gateway(
    env: RuntimeEnv,
) -> Result<std::sync::Arc<dyn dmind_gateway::ModelGateway>, String> {
    wellos_server::runtime::fixtures_allowed(env, "dmind-eval --provider fake")
        .map_err(|e| e.to_string())?;
    Ok(std::sync::Arc::new(dmind_gateway::fake::FakeProvider::new()))
}

#[cfg(not(feature = "dev-fixtures"))]
fn fixture_gateway(
    _env: RuntimeEnv,
) -> Result<std::sync::Arc<dyn dmind_gateway::ModelGateway>, String> {
    Err(
        "--provider fake requires a dev-fixtures build; production builds have no fixture provider"
            .into(),
    )
}
