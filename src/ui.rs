use std::io::{self, Write};

#[derive(Clone, Copy, Debug, Default)]
pub struct Logger;

impl Logger {
    pub fn infof(&self, msg: &str) {
        let _ = writeln!(io::stderr(), "[wrt] {msg}");
    }

    pub fn errorf(&self, msg: &str) {
        let _ = writeln!(io::stderr(), "[wrt] ERROR: {msg}");
    }
}
