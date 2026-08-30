/// Prints "Hello, world!" to stdout and returns the message.
fn hello_world() -> String {
    let message = String::from("Hello, world!");
    println!("{message}");
    message
}

fn main() {
    hello_world();
    println!("{}", answer());
}

/// One-line function: returns the answer to life, the universe, and everything.
fn answer() -> i32 { 42 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prints_and_returns_hello_world() {
        assert_eq!(hello_world(), "Hello, world!");
    }
}
