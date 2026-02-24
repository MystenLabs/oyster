use serde::Serialize;

pub struct Output {
    pub json: bool,
}

impl Output {
    pub fn print<T: Serialize>(&self, value: &T, human_fn: impl FnOnce(&T)) {
        if self.json {
            println!(
                "{}",
                serde_json::to_string_pretty(value).expect("serialization should not fail")
            );
        } else {
            human_fn(value);
        }
    }

    pub fn success(&self, msg: &str) {
        if !self.json {
            println!("{msg}");
        }
    }
}

pub fn error(msg: &str) {
    eprintln!("error: {msg}");
}
