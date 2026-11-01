//! Capability-limited WASI preview1 host for Rust reflexes.
//!
//! No host filesystem, environment, stdin, sockets or process handles are
//! inherited. Both historical import names are supported. Unsupported calls
//! return explicit WASI errno values; no operation reports a fictitious success.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use wasmi::{Caller, ExternType, Linker, Memory, Module, Val, ValType};

const SUCCESS: i32 = 0;
const BADF: i32 = 8;
const FAULT: i32 = 21;
const INVAL: i32 = 28;
const NOSYS: i32 = 52;
const NOTSUP: i32 = 58;
const SPIPE: i32 = 70;
const NOTCAPABLE: i32 = 76;
pub(crate) const OUTPUT_LIMIT: usize = crate::resources::MAX_OUTPUT_BYTES;
const WALL_TIME_LIMIT: Duration = Duration::from_secs(5);

pub(crate) struct Context {
    pub(crate) limits: wasmi::StoreLimits,
    pub(crate) stdout: Vec<u8>,
    pub(crate) memory: Option<Memory>,
    args: Vec<Vec<u8>>,
    closed: [bool; 3],
    output_bytes: usize,
    started: Instant,
}

impl Context {
    pub(crate) fn new(argument: &str) -> Result<Self, String> {
        if argument.contains('\0') || argument.len() > OUTPUT_LIMIT {
            return Err("reflex argument contains NUL or exceeds 16 MiB".into());
        }
        Ok(Self {
            limits: crate::resources::store_limits(),
            stdout: Vec::new(),
            memory: None,
            args: vec![b"reflex\0".to_vec(), [argument.as_bytes(), &[0]].concat()],
            closed: [false; 3],
            output_bytes: 0,
            started: Instant::now(),
        })
    }
}

fn memory(caller: &Caller<'_, Context>) -> Result<Memory, i32> {
    caller.data().memory.ok_or(FAULT)
}

fn address(value: i32) -> usize {
    // WASI pointers are unsigned wasm32 addresses, represented as i32 by the VM.
    value as u32 as usize
}

fn write(caller: &mut Caller<'_, Context>, ptr: i32, data: &[u8]) -> Result<(), i32> {
    memory(caller)?
        .write(caller, address(ptr), data)
        .map_err(|_| FAULT)
}

fn write_u32(caller: &mut Caller<'_, Context>, ptr: i32, value: u32) -> Result<(), i32> {
    write(caller, ptr, &value.to_le_bytes())
}

fn arg(values: &[Val], index: usize) -> Result<i32, i32> {
    values.get(index).and_then(Val::i32).ok_or(INVAL)
}

fn fd_open(context: &Context, fd: i32) -> bool {
    usize::try_from(fd)
        .ok()
        .and_then(|fd| context.closed.get(fd))
        .is_some_and(|closed| !closed)
}

fn fd_write(caller: &mut Caller<'_, Context>, values: &[Val]) -> Result<(), i32> {
    let fd = arg(values, 0)?;
    if !matches!(fd, 1 | 2) || !fd_open(caller.data(), fd) {
        return Err(BADF);
    }
    let vectors = address(arg(values, 1)?);
    let count = address(arg(values, 2)?);
    let written = arg(values, 3)?;
    // Bound host-side loop work independently of guest instruction fuel.
    if count > 65_536 {
        return Err(INVAL);
    }
    let memory = memory(caller)?;
    let mut collected = Vec::new();
    for index in 0..count {
        let offset = index
            .checked_mul(8)
            .and_then(|v| vectors.checked_add(v))
            .ok_or(FAULT)?;
        let mut iovec = [0; 8];
        memory
            .read(&*caller, offset, &mut iovec)
            .map_err(|_| FAULT)?;
        let ptr = u32::from_le_bytes([iovec[0], iovec[1], iovec[2], iovec[3]]) as usize;
        let len = u32::from_le_bytes([iovec[4], iovec[5], iovec[6], iovec[7]]) as usize;
        let total = collected.len().checked_add(len).ok_or(INVAL)?;
        if total > OUTPUT_LIMIT.saturating_sub(caller.data().output_bytes) {
            return Err(NOTCAPABLE);
        }
        let previous = collected.len();
        collected.resize(total, 0);
        memory
            .read(&*caller, ptr, &mut collected[previous..])
            .map_err(|_| FAULT)?;
    }
    write_u32(
        caller,
        written,
        u32::try_from(collected.len()).map_err(|_| INVAL)?,
    )?;
    caller.data_mut().output_bytes += collected.len();
    if fd == 1 {
        caller.data_mut().stdout.extend(collected);
    }
    // stderr is a bounded sink, never the host's stderr.
    Ok(())
}

fn clock_nanos(context: &Context, id: u32) -> Result<u64, i32> {
    let duration = match id {
        0 => SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| INVAL)?,
        1 => context.started.elapsed(),
        _ => return Err(NOTSUP),
    };
    u64::try_from(duration.as_nanos()).map_err(|_| INVAL)
}

fn poll(caller: &mut Caller<'_, Context>, values: &[Val]) -> Result<(), i32> {
    let input = address(arg(values, 0)?);
    let output = arg(values, 1)?;
    let count = address(arg(values, 2)?);
    let written = arg(values, 3)?;
    if count == 0 || count > 1024 {
        return Err(INVAL);
    }
    let memory = memory(caller)?;
    let mut subscriptions = Vec::with_capacity(count);
    let mut first = None;
    for index in 0..count {
        let mut bytes = [0u8; 48];
        memory
            .read(
                &*caller,
                input.checked_add(index * 48).ok_or(FAULT)?,
                &mut bytes,
            )
            .map_err(|_| FAULT)?;
        let mut event = [0u8; 32];
        event[..8].copy_from_slice(&bytes[..8]);
        event[10] = bytes[8];
        let delay = match bytes[8] {
            0 => {
                let id = u32::from_le_bytes(bytes[16..20].try_into().map_err(|_| INVAL)?);
                let timeout = u64::from_le_bytes(bytes[24..32].try_into().map_err(|_| INVAL)?);
                let flags = u16::from_le_bytes(bytes[40..42].try_into().map_err(|_| INVAL)?);
                if flags > 1 {
                    return Err(INVAL);
                }
                match clock_nanos(caller.data(), id) {
                    Ok(now) => Duration::from_nanos(if flags == 1 {
                        timeout.saturating_sub(now)
                    } else {
                        timeout
                    }),
                    Err(error) => {
                        event[8..10].copy_from_slice(&(error as u16).to_le_bytes());
                        Duration::ZERO
                    }
                }
            }
            1 | 2 => {
                let fd = i32::from_le_bytes(bytes[16..20].try_into().map_err(|_| INVAL)?);
                if !fd_open(caller.data(), fd)
                    || (bytes[8] == 1 && fd != 0)
                    || (bytes[8] == 2 && fd == 0)
                {
                    event[8..10].copy_from_slice(&(BADF as u16).to_le_bytes());
                } else if bytes[8] == 1 {
                    event[24..26].copy_from_slice(&1u16.to_le_bytes()); // EOF/hangup
                } else {
                    event[16..24].copy_from_slice(
                        &(OUTPUT_LIMIT.saturating_sub(caller.data().output_bytes) as u64)
                            .to_le_bytes(),
                    );
                }
                Duration::ZERO
            }
            _ => return Err(INVAL),
        };
        first = Some(first.map_or(delay, |current: Duration| current.min(delay)));
        subscriptions.push((delay, event));
    }
    let delay = first.ok_or(INVAL)?;
    if delay > WALL_TIME_LIMIT.saturating_sub(caller.data().started.elapsed()) {
        return Err(NOTCAPABLE);
    }
    let began = Instant::now();
    if !delay.is_zero() {
        std::thread::sleep(delay);
    }
    let elapsed = began.elapsed();
    let mut events = Vec::new();
    for (deadline, event) in subscriptions {
        if deadline <= elapsed {
            events.extend_from_slice(&event);
        }
    }
    write(caller, output, &events)?;
    write_u32(
        caller,
        written,
        u32::try_from(events.len() / 32).map_err(|_| INVAL)?,
    )
}

fn dispatch(caller: &mut Caller<'_, Context>, name: &str, values: &[Val]) -> Result<(), i32> {
    match name {
        "args_sizes_get" => {
            let bytes: usize = caller.data().args.iter().map(Vec::len).sum();
            write_u32(caller, arg(values, 0)?, 2)?;
            write_u32(
                caller,
                arg(values, 1)?,
                u32::try_from(bytes).map_err(|_| INVAL)?,
            )
        }
        "args_get" => {
            let pointers = address(arg(values, 0)?);
            let mut buffer = address(arg(values, 1)?);
            let args = caller.data().args.clone();
            for (index, value) in args.iter().enumerate() {
                let destination = pointers.checked_add(index * 4).ok_or(FAULT)?;
                let pointer = u32::try_from(buffer).map_err(|_| FAULT)?;
                let memory = memory(caller)?;
                memory
                    .write(&mut *caller, destination, &pointer.to_le_bytes())
                    .map_err(|_| FAULT)?;
                memory
                    .write(&mut *caller, buffer, value)
                    .map_err(|_| FAULT)?;
                buffer = buffer.checked_add(value.len()).ok_or(FAULT)?;
            }
            Ok(())
        }
        "environ_sizes_get" => {
            write_u32(caller, arg(values, 0)?, 0)?;
            write_u32(caller, arg(values, 1)?, 0)
        }
        "environ_get" => Ok(()), // Empty environment has no bytes or pointer entries.
        "fd_write" => fd_write(caller, values),
        "poll_oneoff" => poll(caller, values),
        "fd_read" => {
            if arg(values, 0)? != 0 || !fd_open(caller.data(), 0) {
                return Err(BADF);
            }
            write_u32(caller, arg(values, 3)?, 0) // Uninherited stdin is EOF.
        }
        "fd_close" => {
            let fd = arg(values, 0)?;
            if !fd_open(caller.data(), fd) {
                return Err(BADF);
            }
            caller.data_mut().closed[fd as usize] = true;
            Ok(())
        }
        "fd_fdstat_get" => {
            let fd = arg(values, 0)?;
            if !fd_open(caller.data(), fd) {
                return Err(BADF);
            }
            let mut stat = [0u8; 24];
            stat[0] = 2; // __WASI_FILETYPE_CHARACTER_DEVICE
            let rights: u64 = if fd == 0 { 1 << 1 } else { 1 << 6 };
            stat[8..16].copy_from_slice(&rights.to_le_bytes());
            write(caller, arg(values, 1)?, &stat)
        }
        "fd_filestat_get" => {
            if !fd_open(caller.data(), arg(values, 0)?) {
                return Err(BADF);
            }
            let mut stat = [0u8; 64];
            stat[16] = 2; // character device
            stat[24..32].copy_from_slice(&1u64.to_le_bytes());
            write(caller, arg(values, 1)?, &stat)
        }
        "fd_seek" | "fd_tell" => {
            if fd_open(caller.data(), arg(values, 0)?) {
                Err(SPIPE)
            } else {
                Err(BADF)
            }
        }
        "fd_sync" | "fd_datasync" => {
            if matches!(arg(values, 0)?, 1 | 2) && fd_open(caller.data(), arg(values, 0)?) {
                Ok(())
            } else {
                Err(BADF)
            }
        }
        "fd_prestat_get" | "fd_prestat_dir_name" => Err(BADF),
        "clock_time_get" | "clock_res_get" => {
            let id = arg(values, 0)?;
            if !matches!(id, 0 | 1) {
                return Err(NOTSUP);
            }
            let (destination, nanos) = if name == "clock_res_get" {
                (arg(values, 1)?, 1_000_000)
            } else {
                let duration = if id == 0 {
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|_| INVAL)?
                } else {
                    caller.data().started.elapsed()
                };
                (
                    arg(values, 2)?,
                    u64::try_from(duration.as_nanos()).map_err(|_| INVAL)?,
                )
            };
            write(caller, destination, &nanos.to_le_bytes())
        }
        "random_get" => {
            let len = address(arg(values, 1)?);
            if len > OUTPUT_LIMIT {
                return Err(INVAL);
            }
            let mut bytes = vec![0; len];
            getrandom::fill(&mut bytes).map_err(|_| INVAL)?;
            write(caller, arg(values, 0)?, &bytes)
        }
        "sched_yield" => {
            std::thread::yield_now();
            Ok(())
        }
        // No preopens, sockets or writable filesystem capabilities exist.
        name if name.starts_with("path_") || name.starts_with("sock_") => Err(BADF),
        name if name.starts_with("fd_") => Err(NOTCAPABLE),
        // Polling, signals and other unimplemented capabilities fail explicitly.
        _ => Err(NOSYS),
    }
}

pub(crate) fn link(linker: &mut Linker<Context>, module: &Module) -> Result<(), wasmi::Error> {
    let mut seen = std::collections::BTreeSet::new();
    for import in module.imports() {
        if !matches!(import.module(), "wasi_snapshot_preview1" | "wasi_unstable") {
            continue;
        }
        let name = import.name().to_owned();
        if !seen.insert((import.module().to_owned(), name.clone())) {
            continue;
        }
        let ExternType::Func(signature) = import.ty() else {
            return Err(wasmi::Error::new("WASI imports must be functions"));
        };
        let signature = signature.clone();
        let expected: Option<&[ValType]> = match name.as_str() {
            "args_get" | "args_sizes_get" | "environ_get" | "environ_sizes_get"
            | "fd_fdstat_get" | "fd_filestat_get" | "fd_prestat_get" | "fd_tell"
            | "clock_res_get" | "random_get" => Some(&[ValType::I32, ValType::I32]),
            "fd_read" | "fd_write" | "poll_oneoff" => Some(&[ValType::I32; 4]),
            "fd_prestat_dir_name" => Some(&[ValType::I32; 3]),
            "fd_close" | "fd_sync" | "fd_datasync" | "proc_exit" => Some(&[ValType::I32]),
            "fd_seek" => Some(&[ValType::I32, ValType::I64, ValType::I32, ValType::I32]),
            "clock_time_get" => Some(&[ValType::I32, ValType::I64, ValType::I32]),
            "sched_yield" => Some(&[]),
            _ => None,
        };
        let expected_results: &[ValType] = if name == "proc_exit" {
            &[]
        } else {
            &[ValType::I32]
        };
        if expected.is_some_and(|types| signature.params() != types)
            || signature.results() != expected_results
        {
            return Err(wasmi::Error::new(format!(
                "invalid WASI signature for {name}"
            )));
        }
        linker.func_new(
            import.module(),
            import.name(),
            signature,
            move |mut caller, values, results| {
                if name == "proc_exit" {
                    return Err(wasmi::Error::i32_exit(
                        arg(values, 0)
                            .map_err(|_| wasmi::Error::new("invalid proc_exit signature"))?,
                    ));
                }
                if results.len() != 1 || results[0].i32().is_none() {
                    return Err(wasmi::Error::new("invalid WASI errno return signature"));
                }
                results[0] = Val::I32(
                    dispatch(&mut caller, &name, values)
                        .err()
                        .unwrap_or(SUCCESS),
                );
                Ok(())
            },
        )?;
    }
    Ok(())
}
