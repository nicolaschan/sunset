import gleam/result
import gleam/uri.{type Uri}
import lustre/effect.{type Effect}
import modem
import sutro/model.{type Msg, type Route, Dev, Home, RouteChanged}

fn on_url_change(uri: Uri) -> Msg {
  let route = case uri.path_segments(uri.path) {
    ["dev"] -> Dev
    _ -> Home
  }
  RouteChanged(route)
}

pub fn init_route() -> Route {
  modem.initial_uri()
  |> result.map(fn(uri) { uri.path_segments(uri.path) })
  |> fn(path) {
    case path {
      Ok(["dev"]) -> Dev
      _ -> Home
    }
  }
}

pub fn init() -> Effect(Msg) {
  modem.init(on_url_change)
}
