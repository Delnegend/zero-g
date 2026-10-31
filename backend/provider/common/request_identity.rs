use std::sync::{Arc, Mutex};

/// Non-secret identities observed while serving a single request.
#[derive(Clone, Default)]
pub(crate) struct RequestIdentity(Arc<Mutex<IdentitySnapshot>>);

/// Stable IDs and names as they were when the request was served.
#[derive(Clone, Default)]
pub(crate) struct IdentitySnapshot {
    /// Stable authenticated virtual key ID.
    pub(crate) virtual_key_id: Option<String>,
    /// Virtual key name at authentication time.
    pub(crate) virtual_key_name: Option<String>,
    /// Stable successfully selected credential ID.
    pub(crate) credential_id: Option<String>,
    /// Selected credential name at request time.
    pub(crate) credential_name: Option<String>,
}

impl RequestIdentity {
    /// Record the authenticated virtual key.
    pub(crate) fn set_virtual_key(&self, id: &str, name: &str) {
        let mut snapshot = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.virtual_key_id = Some(id.to_owned());
        snapshot.virtual_key_name = Some(name.to_owned());
    }

    /// Record the successfully selected credential.
    pub(crate) fn set_credential(&self, id: &str, name: &str) {
        let mut snapshot = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.credential_id = Some(id.to_owned());
        snapshot.credential_name = Some(name.to_owned());
    }

    /// Copy the identities observed so far.
    pub(crate) fn snapshot(&self) -> IdentitySnapshot {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
