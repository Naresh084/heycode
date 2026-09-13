
AuthorizationService::credential_state exposes only safe credential metadata so a connection can reuse configured credentials before requesting sign-in.

Operation-scoped endpoint authorization uses `authorize_once`: it executes a caller-owned flow through the same cancellation/validation/authoritative commit path without installing a registry row. Its regression verifies committed readback and an unchanged flow catalog.
