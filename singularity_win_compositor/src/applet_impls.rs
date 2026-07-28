use crate::WindowsApplet;
use singularity_common::utils::tree::world_tree::{WorldTree, WorldTreePath};
use singularity_sar::applet::BasicApplet;
use singularity_sttk::{
    nodular_applet::NodularApplet, standard_keybinds::handle_standard_keybinds,
};
use sonamu_ui::display_units::DisplayContainerSize;

impl BasicApplet for WindowsApplet {
    fn handle_ui_event(&mut self, ui_event: sonamu_ui::ui_event::UIEvent) {
        if handle_standard_keybinds(&ui_event, &self.hook) {
            return;
        }

        // The compositor thread exits (dropping the receiver) when the embedded
        // app closes; input to a dead applet is just ignored
        let _ = self.input_sender.send(ui_event);

        self.hook.damage_window();
    }

    fn get_window(
        &self,
        _container_size: DisplayContainerSize,
    ) -> sonamu_ui::ui_element::UIElement {
        if let Ok(image) = self.image.lock()
            && let Some(ref image) = *image
        {
            sonamu_ui::ui_element::UIElement::Image(image.clone())
        } else {
            sonamu_ui::ui_element::UIElement::from("Windows app loading...".to_string())
        }
    }
}
impl NodularApplet for WindowsApplet {
    fn handle_nodular_event(
        &mut self,
        _nodular_event: singularity_sttk::nodular_applet::NodularEvent,
    ) {
        todo!()
    }

    fn get_treeview(&self) -> singularity_common::utils::tree::world_tree::WorldTree<String> {
        // TODO: actual name (the embedded window's title)
        WorldTree::Base("Windows App".to_string())
    }

    fn get_focus_path(&self) -> singularity_common::utils::tree::world_tree::WorldTreePath {
        WorldTreePath::new_into()
    }
}
