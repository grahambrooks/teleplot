//! Dev-only: writes a synthetic software-delivery dataset to Elasticsearch.
//!
//! Teleplot never writes to Elasticsearch. This seeder exists only so fixtures,
//! tests and demos have realistic data. It only touches `delivery-*` indexes.
//!
//! ```text
//! cargo run --example seed -- --es http://localhost:9208 [--days 182] [--end 2026-10-01] [--seed 42] [--scale 1]
//! ```
//!
//! The data is deterministic for a given `--seed`, `--days` and `--end`.
//! Durations are long-tailed (log-normal), teams have distinct performance
//! profiles, one team improves over the period, and AI-assisted work grows.

use std::collections::HashMap;

use jiff::{Span, Timestamp, ToSpan, Zoned, civil::Date, tz::TimeZone};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rand_distr::{Distribution, LogNormal, Poisson};
use serde_json::{Value, json};

const INDEXES: [&str; 4] = [
    "delivery-prs",
    "delivery-deployments",
    "delivery-incidents",
    "delivery-workitems",
];

#[derive(Clone, Copy)]
struct Profile {
    /// Production deploys per repo per working day.
    deploys_per_day: f64,
    /// Median PR cycle time (first commit → deployed), hours.
    cycle_hours: f64,
    /// Median change lead time for a deployment, hours.
    lead_hours: f64,
    change_fail_rate: f64,
    /// Median failed-deployment recovery time, hours.
    recovery_hours: f64,
    /// Median work-item flow time, days.
    flow_days: f64,
}

const ELITE: Profile = Profile {
    deploys_per_day: 1.6,
    cycle_hours: 18.0,
    lead_hours: 6.0,
    change_fail_rate: 0.05,
    recovery_hours: 0.8,
    flow_days: 3.0,
};
const HIGH: Profile = Profile {
    deploys_per_day: 0.5,
    cycle_hours: 40.0,
    lead_hours: 30.0,
    change_fail_rate: 0.10,
    recovery_hours: 5.0,
    flow_days: 5.0,
};
const MEDIUM: Profile = Profile {
    deploys_per_day: 0.25,
    cycle_hours: 100.0,
    lead_hours: 120.0,
    change_fail_rate: 0.18,
    recovery_hours: 22.0,
    flow_days: 9.0,
};
const LOW: Profile = Profile {
    deploys_per_day: 0.08,
    cycle_hours: 230.0,
    lead_hours: 500.0,
    change_fail_rate: 0.30,
    recovery_hours: 90.0,
    flow_days: 16.0,
};

struct Team {
    group: &'static str,
    name: &'static str,
    repos: &'static [&'static str],
    headcount: u32,
    tz: &'static str,
    profile: Profile,
    /// Multiplier on durations at the start of the period, easing to 1/this
    /// at the end. 1.0 means no trend.
    improvement: f64,
}

const TEAMS: &[Team] = &[
    Team {
        group: "payments",
        name: "checkout",
        repos: &["checkout-web", "checkout-api"],
        headcount: 8,
        tz: "America/Los_Angeles",
        profile: ELITE,
        improvement: 1.0,
    },
    Team {
        group: "payments",
        name: "billing",
        repos: &["billing-service", "invoicing"],
        headcount: 6,
        tz: "America/Los_Angeles",
        profile: HIGH,
        improvement: 1.0,
    },
    Team {
        group: "platform",
        name: "infra",
        repos: &["terraform-live", "k8s-platform", "ci-runners"],
        headcount: 5,
        tz: "Europe/London",
        profile: HIGH,
        improvement: 1.0,
    },
    Team {
        group: "platform",
        name: "observability",
        repos: &["telemetry-pipeline"],
        headcount: 4,
        tz: "Europe/London",
        profile: MEDIUM,
        improvement: 1.0,
    },
    Team {
        group: "growth",
        name: "onboarding",
        repos: &["signup-flow", "email-service"],
        headcount: 7,
        tz: "America/New_York",
        profile: MEDIUM,
        improvement: 1.0,
    },
    Team {
        group: "growth",
        name: "experiments",
        repos: &["exp-framework"],
        headcount: 3,
        tz: "America/New_York",
        profile: LOW,
        improvement: 1.8,
    },
    Team {
        group: "core",
        name: "identity",
        repos: &["auth-service", "user-directory"],
        headcount: 9,
        tz: "Asia/Kolkata",
        profile: HIGH,
        improvement: 1.0,
    },
    Team {
        group: "core",
        name: "data",
        repos: &["etl-jobs", "warehouse-models", "reporting-api"],
        headcount: 6,
        tz: "Asia/Kolkata",
        profile: LOW,
        improvement: 1.0,
    },
];

struct Args {
    es: String,
    days: i32,
    end: Date,
    seed: u64,
    scale: u32,
    /// Prepended to every index name, e.g. `perf-` for a large copy.
    prefix: String,
}

fn parse_args() -> Args {
    let mut args = Args {
        es: "http://localhost:9208".into(),
        days: 182,
        end: Zoned::now().date(),
        seed: 42,
        scale: 1,
        prefix: String::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--es" => args.es = value.trim_end_matches('/').into(),
            "--days" => args.days = value.parse().expect("--days: integer"),
            "--end" => args.end = value.parse().expect("--end: YYYY-MM-DD"),
            "--seed" => args.seed = value.parse().expect("--seed: integer"),
            "--scale" => args.scale = value.parse().expect("--scale: integer"),
            "--prefix" => args.prefix = value,
            other => panic!("unknown flag {other}"),
        }
    }
    args
}

/// Log-normal sampler parameterised by its median, which is how delivery
/// durations are usually reported.
fn lognormal(median: f64, sigma: f64) -> LogNormal<f64> {
    LogNormal::new(median.ln(), sigma).expect("valid log-normal")
}

fn hours(h: f64) -> Span {
    Span::new().seconds((h * 3600.0).round() as i64)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// A random moment during working hours (09:00–18:00) on `day` in `tz`.
fn working_time(rng: &mut ChaCha8Rng, day: Date, tz: &TimeZone) -> Timestamp {
    let minute: i16 = rng.random_range(9 * 60..18 * 60);
    day.at(
        (minute / 60) as i8,
        (minute % 60) as i8,
        rng.random_range(0..60),
        0,
    )
    .to_zoned(tz.clone())
    .expect("valid local time")
    .timestamp()
}

struct Generator {
    rng: ChaCha8Rng,
    start: Date,
    days: i32,
    end_ts: Timestamp,
    scale: f64,
    docs: HashMap<&'static str, Vec<Value>>,
    next_id: u64,
}

impl Generator {
    fn id(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}-{:06}", self.next_id)
    }

    /// 0.0 at the start of the period, 1.0 at the end.
    fn progress(&self, day: i32) -> f64 {
        day as f64 / (self.days - 1).max(1) as f64
    }

    /// Duration multiplier for a team on a given day of the period.
    fn trend(&self, team: &Team, day: i32) -> f64 {
        let start = team.improvement;
        let end = 1.0 / team.improvement;
        start + (end - start) * self.progress(day)
    }

    fn push(&mut self, index: &'static str, doc: Value) {
        self.docs.entry(index).or_default().push(doc);
    }

    /// One team's documents. Teams are generated (and indexed) one at a
    /// time so large `--scale` runs need not hold everything in memory.
    fn generate_team(&mut self, team: &Team) {
        {
            let tz = TimeZone::get(team.tz).expect("known time zone");
            for day in 0..self.days {
                let date = self.start + day.days();
                let weekend = date.weekday().to_monday_one_offset() > 5;
                let activity = if weekend { 0.08 } else { 1.0 } * self.scale;
                self.prs(team, &tz, day, date, activity);
                for repo in team.repos {
                    self.deployments(team, repo, &tz, day, date, activity);
                }
                if date.weekday().to_monday_one_offset() == 1 {
                    self.work_items(team, &tz, day, date);
                }
            }
        }
    }

    fn prs(&mut self, team: &Team, tz: &TimeZone, day: i32, date: Date, activity: f64) {
        let rate = team.headcount as f64 * 0.45 * activity;
        let count = Poisson::new(rate).unwrap().sample(&mut self.rng) as u32;
        let trend = self.trend(team, day);
        let ai_share = 0.15 + 0.5 * self.progress(day);
        let median = team.profile.cycle_hours * trend;

        for _ in 0..count {
            let ai_assisted = self.rng.random_bool(ai_share);
            let ai_factor = if ai_assisted { 0.8 } else { 1.0 };
            let coding = lognormal(median * 0.35 * ai_factor, 0.9).sample(&mut self.rng);
            let pickup = lognormal(median * 0.15, 1.2).sample(&mut self.rng);
            let review = lognormal(median * 0.30, 1.0).sample(&mut self.rng);
            let deploy = lognormal(median * 0.20, 0.8).sample(&mut self.rng);

            let first_commit = working_time(&mut self.rng, date, tz);
            let opened = first_commit + hours(coding);
            let first_review = opened + hours(pickup);
            let merged = first_review + hours(review);
            let deployed = merged + hours(deploy);

            let closed_unmerged = self.rng.random_bool(0.05);
            let state = if opened > self.end_ts {
                continue;
            } else if closed_unmerged {
                "closed"
            } else if merged > self.end_ts {
                "open"
            } else {
                "merged"
            };
            let merged_fields = state == "merged";
            let deployed_fields = merged_fields && deployed <= self.end_ts;

            let id = self.id("pr");
            let repo = team.repos[self.rng.random_range(0..team.repos.len())];
            let author = format!(
                "{}-{:02}",
                team.name,
                self.rng.random_range(0..team.headcount)
            );
            let lines = lognormal(120.0, 1.1).sample(&mut self.rng).round();
            self.push(
                "delivery-prs",
                json!({
                    "pr_id": id,
                    "group": team.group,
                    "team": team.name,
                    "repo": repo,
                    "author": author,
                    "state": state,
                    "ai_assisted": ai_assisted,
                    "lines_changed": lines,
                    "first_commit_at": first_commit.to_string(),
                    "created_at": opened.to_string(),
                    "first_review_at": (first_review <= self.end_ts).then(|| first_review.to_string()),
                    "merged_at": merged_fields.then(|| merged.to_string()),
                    "deployed_at": deployed_fields.then(|| deployed.to_string()),
                    "coding_hours": round2(coding),
                    "pickup_hours": round2(pickup),
                    "review_hours": merged_fields.then(|| round2(review)),
                    "deploy_hours": deployed_fields.then(|| round2(deploy)),
                    "cycle_time_hours": deployed_fields.then(|| round2(coding + pickup + review + deploy)),
                }),
            );
        }
    }

    fn deployments(
        &mut self,
        team: &Team,
        repo: &'static str,
        tz: &TimeZone,
        day: i32,
        date: Date,
        activity: f64,
    ) {
        let rate = team.profile.deploys_per_day * activity;
        let count = Poisson::new(rate).unwrap().sample(&mut self.rng) as u32;
        let trend = self.trend(team, day);

        for _ in 0..count {
            let at = working_time(&mut self.rng, date, tz);
            if at > self.end_ts {
                continue;
            }
            let failed = self
                .rng
                .random_bool((team.profile.change_fail_rate * trend).min(0.9));
            let id = self.id("deploy");
            let lead = lognormal(team.profile.lead_hours * trend, 0.9).sample(&mut self.rng);
            let commits = 1 + Poisson::new(4.0).unwrap().sample(&mut self.rng) as u32;
            self.push(
                "delivery-deployments",
                json!({
                    "deployment_id": id,
                    "group": team.group,
                    "team": team.name,
                    "repo": repo,
                    "environment": "production",
                    "deployed_at": at.to_string(),
                    "result": if failed { "failure" } else { "success" },
                    "is_rework": false,
                    "change_lead_time_hours": round2(lead),
                    "commit_count": commits,
                }),
            );
            if failed {
                self.incident(team, repo, at, Some(id), trend);
            }
        }

        // Incidents not caused by a deployment (infrastructure, dependencies).
        if self.rng.random_bool(0.004 * activity.min(1.0)) {
            let at = working_time(&mut self.rng, date, tz);
            if at <= self.end_ts {
                self.incident(team, repo, at, None, trend);
            }
        }
    }

    /// An incident, plus the unplanned rework deployment that resolves it.
    fn incident(
        &mut self,
        team: &Team,
        repo: &'static str,
        cause_at: Timestamp,
        cause: Option<String>,
        trend: f64,
    ) {
        let started = cause_at + hours(self.rng.random_range(0.05..2.0));
        let recovery = lognormal(team.profile.recovery_hours * trend, 1.0).sample(&mut self.rng);
        let resolved = started + hours(recovery);
        let severity = ["sev1", "sev2", "sev3"][match self.rng.random_range(0..100) {
            0..10 => 0,
            10..45 => 1,
            _ => 2,
        }];
        let id = self.id("inc");
        let is_resolved = resolved <= self.end_ts;
        self.push(
            "delivery-incidents",
            json!({
                "incident_id": id,
                "group": team.group,
                "team": team.name,
                "repo": repo,
                "severity": severity,
                "created_at": started.to_string(),
                "resolved_at": is_resolved.then(|| resolved.to_string()),
                "recovery_hours": is_resolved.then(|| round2(recovery)),
                "caused_by_deployment_id": cause,
            }),
        );
        if is_resolved {
            let id = self.id("deploy");
            self.push(
                "delivery-deployments",
                json!({
                    "deployment_id": id,
                    "group": team.group,
                    "team": team.name,
                    "repo": repo,
                    "environment": "production",
                    "deployed_at": resolved.to_string(),
                    "result": "success",
                    "is_rework": true,
                    "change_lead_time_hours": round2(recovery),
                    "commit_count": 1,
                }),
            );
        }
    }

    fn work_items(&mut self, team: &Team, tz: &TimeZone, day: i32, date: Date) {
        let rate = team.headcount as f64 * 1.1 * self.scale;
        let count = Poisson::new(rate).unwrap().sample(&mut self.rng) as u32;
        let trend = self.trend(team, day);

        for _ in 0..count {
            let kind = match self.rng.random_range(0..100) {
                0..50 => "feature",
                50..75 => "defect",
                75..90 => "debt",
                _ => "risk",
            };
            let offset = self.rng.random_range(0..5).days();
            let created = working_time(&mut self.rng, date + offset, tz);
            let queued = lognormal(2.0, 1.0).sample(&mut self.rng);
            let started = created + hours(queued * 24.0);
            let flow_days = lognormal(team.profile.flow_days * trend, 0.7).sample(&mut self.rng);
            let done = started + hours(flow_days * 24.0);
            let efficiency = self.rng.random_range(0.15..0.65);
            let status = if started > self.end_ts {
                "todo"
            } else if done > self.end_ts {
                "in_progress"
            } else {
                "done"
            };
            if created > self.end_ts {
                continue;
            }
            let finished = status == "done";
            let id = self.id("item");
            let points = [1, 2, 3, 5, 8][self.rng.random_range(0..5)];
            self.push(
                "delivery-workitems",
                json!({
                    "item_id": id,
                    "group": team.group,
                    "team": team.name,
                    "type": kind,
                    "status": status,
                    "story_points": points,
                    "created_at": created.to_string(),
                    "started_at": (status != "todo").then(|| started.to_string()),
                    "done_at": finished.then(|| done.to_string()),
                    "flow_time_days": finished.then(|| round2(flow_days)),
                    "active_days": finished.then(|| round2(flow_days * efficiency)),
                    "flow_efficiency": finished.then(|| round2(efficiency)),
                }),
            );
        }
    }
}

fn mapping(index: &str) -> Value {
    let keyword = json!({ "type": "keyword" });
    let date = json!({ "type": "date" });
    let float = json!({ "type": "float" });
    let mut props = serde_json::Map::new();
    for field in ["group", "team", "repo"] {
        props.insert(field.into(), keyword.clone());
    }
    let (keywords, dates, floats, others): (&[&str], &[&str], &[&str], Value) = match index {
        "delivery-prs" => (
            &["pr_id", "author", "state"],
            &[
                "first_commit_at",
                "created_at",
                "first_review_at",
                "merged_at",
                "deployed_at",
            ],
            &[
                "coding_hours",
                "pickup_hours",
                "review_hours",
                "deploy_hours",
                "cycle_time_hours",
                "lines_changed",
            ],
            json!({ "ai_assisted": { "type": "boolean" } }),
        ),
        "delivery-deployments" => (
            &["deployment_id", "environment", "result"],
            &["deployed_at"],
            &["change_lead_time_hours"],
            json!({ "is_rework": { "type": "boolean" }, "commit_count": { "type": "integer" } }),
        ),
        "delivery-incidents" => (
            &["incident_id", "severity", "caused_by_deployment_id"],
            &["created_at", "resolved_at"],
            &["recovery_hours"],
            json!({}),
        ),
        "delivery-workitems" => (
            &["item_id", "type", "status"],
            &["created_at", "started_at", "done_at"],
            &["flow_time_days", "active_days", "flow_efficiency"],
            json!({ "story_points": { "type": "integer" } }),
        ),
        other => unreachable!("no mapping for {other}"),
    };
    for f in keywords {
        props.insert((*f).into(), keyword.clone());
    }
    for f in dates {
        props.insert((*f).into(), date.clone());
    }
    for f in floats {
        props.insert((*f).into(), float.clone());
    }
    props.extend(others.as_object().unwrap().clone());
    json!({
        "settings": { "number_of_shards": 1, "number_of_replicas": 0 },
        "mappings": { "dynamic": "strict", "properties": props },
    })
}

async fn check(response: reqwest::Response, what: &str) -> Value {
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        panic!("{what} failed ({status}): {body}");
    }
    body
}

#[tokio::main]
async fn main() {
    let args = parse_args();
    let start = args.end - (args.days - 1).days();
    let end_ts = args
        .end
        .tomorrow()
        .expect("valid end")
        .to_zoned(TimeZone::UTC)
        .expect("valid end")
        .timestamp();

    let mut generator = Generator {
        rng: ChaCha8Rng::seed_from_u64(args.seed),
        start,
        days: args.days,
        end_ts,
        scale: args.scale as f64,
        docs: HashMap::new(),
        next_id: 0,
    };

    let client = reqwest::Client::new();
    for index in INDEXES {
        let url = format!("{}/{}{index}", args.es, args.prefix);
        let exists = client.head(&url).send().await.expect("reach Elasticsearch");
        if exists.status().is_success() {
            check(client.delete(&url).send().await.unwrap(), "delete index").await;
        }
        check(
            client.put(&url).json(&mapping(index)).send().await.unwrap(),
            "create index",
        )
        .await;
    }

    let mut totals: HashMap<&str, usize> = HashMap::new();
    for team in TEAMS {
        generator.generate_team(team);
        for index in INDEXES {
            let docs = generator.docs.remove(index).unwrap_or_default();
            *totals.entry(index).or_default() += docs.len();
            for chunk in docs.chunks(5000) {
                let mut body = String::new();
                for doc in chunk {
                    body.push_str("{\"index\":{}}\n");
                    body.push_str(&doc.to_string());
                    body.push('\n');
                }
                let response = client
                    .post(format!("{}/{}{index}/_bulk", args.es, args.prefix))
                    .header("content-type", "application/x-ndjson")
                    .body(body)
                    .send()
                    .await
                    .unwrap();
                let result = check(response, "bulk").await;
                if result["errors"].as_bool() == Some(true) {
                    let first = result["items"]
                        .as_array()
                        .and_then(|items| items.iter().find(|i| i["index"]["error"].is_object()));
                    panic!("bulk indexing into {index} had errors, first: {first:?}");
                }
            }
        }
    }
    for index in INDEXES {
        let url = format!("{}/{}{index}", args.es, args.prefix);
        check(
            client.post(format!("{url}/_refresh")).send().await.unwrap(),
            "refresh",
        )
        .await;
        println!(
            "{}{index}: {} docs",
            args.prefix,
            totals.get(index).copied().unwrap_or(0)
        );
    }
    println!("seeded {start} .. {} (seed {})", args.end, args.seed);
}
