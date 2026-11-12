//! `archon test FILE`: a bench test without the window. It brings up the test's bench (its profile, and the board or
//! robot it simulates), runs the steps, prints each as it finishes, and exits 1 if one fails, so CI can run firmware
//! against firmware_simulator.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use archon_core::profile::{Profile, RestartPolicy};
use archon_core::source::Source;
use archon_msg::Registry;
use archon_ros::Ros;
use archon_sources::Supervisor;

use crate::bench;
use crate::benchtest::{Bench, Runner, State, TestFile};
use crate::robot::{Attach, Spec};

/// How many of each source's last lines a failure prints
const TAIL: usize = 25;

pub fn run(path: &Path, with_ros: bool) -> i32 {
    match run_test(path, with_ros) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(why) => {
            eprintln!("archon test: {why}");
            2
        }
    }
}

fn run_test(path: &Path, with_ros: bool) -> Result<bool, String> {
    let test = TestFile::load(path)?;
    let profile = match &test.bench {
        Some(bench) => Profile::load(&test.resolve(bench)).map_err(|e| format!("{e:#}"))?,
        None => Profile::default(),
    };
    for (key, value) in &profile.env {
        if key.starts_with("RMW_") || key.starts_with("ROS_") {
            // SAFETY: no other threads exist yet
            unsafe { std::env::set_var(key, value) };
        }
    }
    let home = dirs::home_dir().unwrap_or_default();
    let config = dirs::config_dir()
        .map(|dir| dir.join("archon").join("config.toml"))
        .map(|path| bench::Config::load(&path))
        .transpose()?
        .unwrap_or_default();

    let mut sup = Supervisor::new(&profile, Arc::new(|| {}));
    let ros = if with_ros {
        Some(Arc::new(Ros::start(archon_ros::Options::default(), Arc::new(|| {})).map_err(|e| format!("{e:#}"))?))
    } else {
        None
    };
    let registry = Arc::new(Mutex::new(Registry::from_env()));
    bring_up(&test, &mut sup, &config, &profile, &home)?;
    sup.start_autostart();

    println!("{} ({} steps)", test.title(), test.steps.len());
    let mut runner = Runner::new(test);
    while runner.finished.is_none() {
        sup.poll();
        let bench = Bench { sup: &sup, ros: ros.as_ref(), registry: &registry };
        for i in runner.poll(&bench) {
            let step = runner.test.steps[i].describe();
            match &runner.states[i] {
                State::Passed(took) => println!("  ok    {:>2}  {step}  ({:.1} s)", i + 1, took.as_secs_f64()),
                State::Failed(why) => println!("  FAIL  {:>2}  {step}\n              {why}", i + 1),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let passed = runner.passed() == Some(true);
    let skipped = runner.states.iter().filter(|s| **s == State::Skipped).count();
    if skipped > 0 {
        println!("  ({skipped} not run)");
    }
    if !passed {
        // What the bench said, for working out why
        for (_, entry) in sup.live() {
            let lines: Vec<&str> = entry.buffer.iter().map(|l| l.text.as_str()).collect();
            let tail = &lines[lines.len().saturating_sub(TAIL)..];
            if !tail.is_empty() {
                println!("--- {} (last {} lines)", entry.name, tail.len());
                for line in tail {
                    println!("  {line}");
                }
            }
        }
    }
    println!("{} in {:.1} s", if passed { "passed" } else { "FAILED" }, runner.elapsed().as_secs_f64());
    sup.shutdown(Duration::from_secs(3));
    Ok(passed)
}

/// The board or robot the test simulates, as the window would start them
fn bring_up(
    test: &TestFile,
    sup: &mut Supervisor,
    config: &bench::Config,
    profile: &Profile,
    home: &Path,
) -> Result<(), String> {
    let find_agent = |kind: bench::AgentKind| match kind {
        bench::AgentKind::MicroRos => bench::find_agent(config.micro_ros_agent.as_deref(), home),
        bench::AgentKind::NanoRos => bench::find_nano_agent(config.nano_ros_agent.as_deref(), home),
    };
    let add = |sup: &mut Supervisor, spec: Spec| {
        let (name, source, restart): (String, Box<dyn Source>, RestartPolicy) = match spec {
            Spec::Process(spec) => (
                spec.name.clone(),
                Box::new(archon_sources::process::ProcessSource::new(spec.clone(), &profile.env)),
                spec.restart,
            ),
            Spec::Serial(spec) => {
                (spec.name.clone(), Box::new(archon_sources::serial::SerialSource::new(spec)), RestartPolicy::Never)
            }
        };
        let id = sup.add(&name, source, restart, true, "\n");
        sup.start(id);
    };
    let process = |name: &str, cmd: String, cwd: Option<std::path::PathBuf>, restart| {
        Spec::Process(archon_core::profile::ProcessSpec {
            name: name.to_owned(),
            cmd,
            cwd,
            env: Default::default(),
            autostart: true,
            restart,
        })
    };
    if let Some(robot) = &test.robot {
        let simulator = bench::find_simulator(config.firmware_simulator.as_deref(), home)?;
        let robot = crate::robot::load(&test.resolve(robot), config.titan(home).as_deref())?;
        if let Some(why) = robot.problem() {
            return Err(format!("can't run {}: {why}", robot.name));
        }
        let mut taken = Vec::new();
        let plan = crate::robot::plan(&simulator, &robot, "test", &[], || {
            let port = crate::robot::free_udp_port(&taken);
            taken.push(port);
            port
        });
        if !plan.windows.is_empty() {
            return Err(format!(
                "{} has two copies of a board, which need an isolated window each; run it in the window instead",
                robot.name
            ));
        }
        add(
            sup,
            process(&format!("robot {}", robot.name), plan.command, Some(simulator.checkout), RestartPolicy::Never),
        );
        for attach in &plan.here {
            add(sup, attach.spec(find_agent, str::to_owned)?);
        }
    }
    if let Some(build) = &test.simulate {
        let simulator = bench::find_simulator(config.firmware_simulator.as_deref(), home)?;
        let firmware = crate::benchtest::firmware(build, test, config, home)?;
        let board = bench::board_for(&simulator, &firmware.image)
            .ok_or_else(|| format!("none of {}/boards matches {}", simulator.checkout.display(), firmware.name))?;
        let slug: String = firmware.name.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_lowercase();
        let (sim, agent, log) =
            bench::simulation_commands(&simulator, &firmware, &board, &find_agent(bench::AgentKind::MicroRos)?, &slug);
        add(sup, process(&format!("sim {slug}"), sim, Some(simulator.checkout.clone()), RestartPolicy::Never));
        add(sup, process(&format!("agent {slug}"), agent, None, RestartPolicy::OnFailure));
        add(sup, Attach::Console { name: slug.clone(), path: log }.spec(find_agent, str::to_owned)?);
    }
    Ok(())
}
