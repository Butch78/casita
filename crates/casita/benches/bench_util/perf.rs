//! Optional Linux perf FIFO control for scoped benchmark profiling.
use std::io::{self, BufRead, BufReader, Write};

pub struct PerfControl {
    control: std::fs::File,
    ack: BufReader<std::fs::File>,
}

impl PerfControl {
    pub fn open() -> io::Result<Option<Self>> {
        let Some(path) = std::env::var_os("CASITA_BENCH_PERF_CONTROL") else {
            return Ok(None);
        };
        let ack = std::env::var_os("CASITA_BENCH_PERF_ACK")
            .ok_or_else(|| io::Error::other("CASITA_BENCH_PERF_ACK is required"))?;
        Ok(Some(Self {
            control: std::fs::OpenOptions::new().write(true).open(path)?,
            ack: BufReader::new(std::fs::File::open(ack)?),
        }))
    }

    pub fn command(&mut self, command: &str) -> io::Result<()> {
        writeln!(self.control, "{command}")?;
        let mut response = String::new();
        self.ack.read_line(&mut response)?;
        // Some perf versions include a NUL after each acknowledgement.
        if response.trim_matches('\0') != "ack\n" {
            return Err(io::Error::other(format!(
                "perf did not acknowledge {command}: {response:?}"
            )));
        }
        Ok(())
    }
}
