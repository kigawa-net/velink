//! Protocol-independent registry for the future Smithay-backed headless server.
//! Smithay Wayland events will be translated into these state transitions.

use std::collections::BTreeMap;
use velink_core::{LaunchRequestId, ManagedWindowId, RuntimeInstanceId, SurfaceId};

pub mod server;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowRole {
    Toplevel,
    Popup,
    Modal,
    Subsurface,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowState {
    Created,
    Mapped,
    Unmapped,
    Destroyed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedWindow {
    pub id: ManagedWindowId,
    pub generation: u64,
    pub launch_request_id: LaunchRequestId,
    pub runtime_instance_id: RuntimeInstanceId,
    pub parent: Option<ManagedWindowId>,
    pub role: WindowRole,
    pub state: WindowState,
    pub surface_id: Option<SurfaceId>,
    pub size: (u32, u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistryError {
    UnknownWindow,
    ParentMissing,
    InvalidSize,
    GenerationExhausted,
}

#[derive(Default)]
pub struct WindowRegistry {
    next_id: u64,
    windows: BTreeMap<ManagedWindowId, ManagedWindow>,
}

impl WindowRegistry {
    pub fn windows(&self) -> impl Iterator<Item = &ManagedWindow> {
        self.windows.values()
    }
    pub fn get(&self, id: ManagedWindowId) -> Option<&ManagedWindow> {
        self.windows.get(&id)
    }

    pub fn create(
        &mut self,
        launch_request_id: LaunchRequestId,
        runtime_instance_id: RuntimeInstanceId,
        parent: Option<ManagedWindowId>,
        role: WindowRole,
    ) -> Result<ManagedWindowId, RegistryError> {
        if parent.is_some_and(|id| !self.windows.contains_key(&id)) {
            return Err(RegistryError::ParentMissing);
        }
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(RegistryError::GenerationExhausted)?;
        let id = ManagedWindowId(self.next_id);
        self.windows.insert(
            id,
            ManagedWindow {
                id,
                generation: 1,
                launch_request_id,
                runtime_instance_id,
                parent,
                role,
                state: WindowState::Created,
                surface_id: None,
                size: (0, 0),
            },
        );
        Ok(id)
    }

    pub fn map(
        &mut self,
        id: ManagedWindowId,
        surface_id: SurfaceId,
        size: (u32, u32),
    ) -> Result<(), RegistryError> {
        if size.0 == 0 || size.1 == 0 {
            return Err(RegistryError::InvalidSize);
        }
        let window = self
            .windows
            .get_mut(&id)
            .ok_or(RegistryError::UnknownWindow)?;
        window.surface_id = Some(surface_id);
        window.size = size;
        window.state = WindowState::Mapped;
        Ok(())
    }

    pub fn resize(&mut self, id: ManagedWindowId, size: (u32, u32)) -> Result<(), RegistryError> {
        if size.0 == 0 || size.1 == 0 {
            return Err(RegistryError::InvalidSize);
        }
        let window = self
            .windows
            .get_mut(&id)
            .ok_or(RegistryError::UnknownWindow)?;
        window.size = size;
        window.generation = window
            .generation
            .checked_add(1)
            .ok_or(RegistryError::GenerationExhausted)?;
        Ok(())
    }

    pub fn unmap(&mut self, id: ManagedWindowId) -> Result<(), RegistryError> {
        let window = self
            .windows
            .get_mut(&id)
            .ok_or(RegistryError::UnknownWindow)?;
        window.state = WindowState::Unmapped;
        window.surface_id = None;
        Ok(())
    }

    /// Remove a window and all descendants for downstream Surface/Tab cleanup.
    pub fn destroy_tree(
        &mut self,
        id: ManagedWindowId,
    ) -> Result<Vec<ManagedWindow>, RegistryError> {
        if !self.windows.contains_key(&id) {
            return Err(RegistryError::UnknownWindow);
        }
        let mut pending = vec![id];
        let mut removed = Vec::new();
        while let Some(current) = pending.pop() {
            pending.extend(
                self.windows
                    .values()
                    .filter(|w| w.parent == Some(current))
                    .map(|w| w.id),
            );
            if let Some(mut window) = self.windows.remove(&current) {
                window.state = WindowState::Destroyed;
                removed.push(window);
            }
        }
        Ok(removed)
    }
}
