import lustre
import lustre/element/html

pub fn main() {
  let app = lustre.element(html.div([], [html.text("sunset.chat")]))
  let assert Ok(_) = lustre.start(app, "#app", Nil)
  Nil
}
