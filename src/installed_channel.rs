//! Ordinary production launches follow one local selector. The controller supplies trusted
//! release IDs, but launching the installed product needs neither Git nor a repository.
use crate::cli::defs::Commands;
use std::process::ExitCode;

#[cfg(windows)]
#[path = "installed_channel/host_task.rs"]
mod host_task;
#[path = "installed_channel/package.rs"]
mod package;
#[path = "installed_channel/transaction.rs"]
mod transaction;

pub(crate) fn management(command: Option<&Commands>) -> Option<ExitCode> {
    command
        .filter(|cmd| matches!(cmd, Commands::InstallRelease { .. }))
        .map(manage)
}

pub(crate) fn manage(command: &Commands) -> ExitCode {
    let Commands::InstallRelease {
        id,
        previous,
        rollback,
        status,
        expect,
        host_worker,
    } = command
    else {
        return ExitCode::FAILURE;
    };
    let result = transaction::manage(
        transaction::Request {
            release: id.as_deref(),
            previous: previous.as_deref(),
            rollback: *rollback,
            status: *status,
            expected: expect.as_deref(),
        },
        *host_worker,
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Installed release activation failed: {error}");
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn dispatch() -> Option<ExitCode> {
    let exe = std::env::current_exe().ok()?;
    if !transaction::is_dispatcher(&exe) {
        return None;
    }
    let mut args = std::env::args_os().skip(1);
    let command = args.next();
    if command.as_deref() == Some(std::ffi::OsStr::new("install")) && args.next().is_none() {
        return Some(
            match transaction::manage(
                transaction::Request {
                    status: true,
                    ..Default::default()
                },
                false,
            ) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("Installed Susi verification failed: {error}");
                    ExitCode::FAILURE
                }
            },
        );
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("install-release")) {
        return None;
    }
    match transaction::run_in_place(&exe) {
        Ok(true) => return None,
        Ok(false) => {}
        Err(error) => {
            eprintln!("Installed Susi could not launch: {error}");
            return Some(ExitCode::FAILURE);
        }
    }
    match transaction::dispatch(&exe) {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("Installed Susi could not launch: {error}");
            Some(ExitCode::FAILURE)
        }
    }
}
