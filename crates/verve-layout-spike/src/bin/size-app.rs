//! Q2: the smallest GPUI window that draws a text line. With `--features verve` it also links
//! the Verve runtime (default features off) and evaluates one expression.
use gpui::{
    App, AppContext, Context, IntoElement, ParentElement, Render, Styled, Window, WindowOptions,
    div, px, rgb,
};

struct Hello;

impl Render for Hello {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0xffffff))
            .text_color(rgb(0x000000))
            .text_size(px(20.0))
            .child("Hello 你好 مرحبا")
    }
}

fn main() {
    #[cfg(feature = "verve")]
    {
        let mut rt = verve::Runtime::new();
        println!("{:?}", rt.eval("1 + 1").is_ok());
    }
    gpui_platform::application().run(|cx: &mut App| {
        let _ = cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| Hello));
        cx.activate(true);
    });
}
