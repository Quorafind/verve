//! Where `document.cookie` reads and writes: the embedder's cookie jar, when it has one.

/// The cookies a page sees and sets as a script: the embedder's jar, shared with its network
/// stack. A page that has none keeps its cookies in the window for as long as it lives.
pub trait CookieBackend {
    /// What `document.cookie` reads at `url`: the `name=value` pairs of the cookies a script may
    /// see (the `HttpOnly` ones are not among them), joined with `; `.
    fn get(&self, url: &str) -> String;

    /// What `document.cookie = line` does at `url`: `line` is a `Set-Cookie` line, stored if the
    /// jar accepts it from a script.
    fn set(&self, url: &str, line: &str);
}
