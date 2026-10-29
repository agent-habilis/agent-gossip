use std::error::Error;
use std::ffi::OsStr;
use std::process::{Command, Stdio};

/// One child process. Unless `quiet`, `run` and `read` echo the command line
/// to stderr, as the task runner did before it dropped `xshell`.
pub(crate) struct Invocation {
    command: Command,
    quiet: bool,
    silent: bool,
}

impl Invocation {
    pub(crate) fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            command: Command::new(program),
            quiet: false,
            silent: false,
        }
    }

    pub(crate) fn cargo<I>(args: I) -> Self
    where
        I: IntoIterator,
        I::Item: AsRef<OsStr>,
    {
        Self::new("cargo").args(args)
    }

    pub(crate) fn args<I>(mut self, args: I) -> Self
    where
        I: IntoIterator,
        I::Item: AsRef<OsStr>,
    {
        self.command.args(args);
        self
    }

    pub(crate) fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.command.env(key, value);
        self
    }

    pub(crate) fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    /// Discard the child's stdout and stderr, and do not echo the command.
    pub(crate) fn silent(mut self) -> Self {
        self.quiet = true;
        self.silent = true;
        self
    }

    pub(crate) fn run(mut self) -> Result<(), Box<dyn Error>> {
        self.echo();
        if self.silent {
            self.command.stdout(Stdio::null()).stderr(Stdio::null());
        }
        let status = self
            .command
            .status()
            .map_err(|error| self.failure(&error))?;
        if status.success() {
            Ok(())
        } else {
            Err(self.failure(&status))
        }
    }

    /// Stdout of the child, trimmed. Stderr goes to the terminal.
    pub(crate) fn read(mut self) -> Result<String, Box<dyn Error>> {
        self.echo();
        let output = self
            .command
            .stderr(Stdio::inherit())
            .output()
            .map_err(|error| self.failure(&error))?;
        if !output.status.success() {
            return Err(self.failure(&output.status));
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    fn line(&self) -> String {
        let mut words: Vec<String> =
            vec![self.command.get_program().to_string_lossy().into_owned()];
        for arg in self.command.get_args() {
            words.push(arg.to_string_lossy().into_owned());
        }
        words.join(" ")
    }

    fn echo(&self) {
        if !self.quiet {
            eprintln!("$ {}", self.line());
        }
    }

    fn failure(&self, cause: &dyn std::fmt::Display) -> Box<dyn Error> {
        format!("command `{}` failed: {cause}", self.line()).into()
    }
}
