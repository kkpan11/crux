// ANCHOR: app

use crux_core::{
    render::{render, Render},
    App, Command,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub enum Event {
    None,
}

#[derive(Default)]
pub struct Model;

#[derive(Serialize, Deserialize)]
pub struct ViewModel {
    data: String,
}

#[derive(crux_core::macros::Effect)]
#[allow(unused)]
pub struct Capabilities {
    render: Render<Event>,
}

#[derive(Default)]
pub struct Hello;

impl App for Hello {
    type Event = Event;
    type Model = Model;
    type ViewModel = ViewModel;
    type Capabilities = Capabilities;
    type Effect = Effect;

    fn update(
        &self,
        _event: Self::Event,
        _model: &mut Self::Model,
        _caps: &Self::Capabilities,
    ) -> Command<Effect, Event> {
        render()
    }

    fn view(&self, _model: &Self::Model) -> Self::ViewModel {
        ViewModel {
            data: "Hello World".to_string(),
        }
    }
}

// ANCHOR_END: app

// ANCHOR: test
#[cfg(test)]
mod tests {
    use super::*;
    use crux_core::testing::AppTester;

    #[test]
    fn hello_says_hello_world() {
        let hello = AppTester::<Hello>::default();
        let mut model = Model;

        // Call 'update' and request effects
        let update = hello.update(Event::None, &mut model);

        // Check update asked us to `Render`
        update.expect_one_effect().expect_render();

        // Make sure the view matches our expectations
        let actual_view = &hello.view(&model).data;
        let expected_view = "Hello World";
        assert_eq!(actual_view, expected_view);
    }
}

// ANCHOR_END: test
