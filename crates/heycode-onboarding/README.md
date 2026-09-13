# heycode-onboarding

## Connection setup update — 2026-09-05

The welcome page directly offers subscription, local model, API provider and managed-cloud connection families. Assistant, connection and model choices are supplied by product services. Model-page Back returns to the originating connection page. The full-screen view separates bold option titles from indented descriptions, highlights the selection and reserves a visible keyboard footer.

Provider, assistant and model lists support bounded case-insensitive search. Confirmation resolves the filtered identity; an empty result has no action. Page changes clear the query.

Connection pages are separate from authorization methods and support local providers with no credential. Saved connection repair starts at Reconnect with the provider retained and a choice to switch connections.

The local server URL page owns its input separately from search. Ctrl+U clears it, Backspace edits it and Enter requests discovery. Returning from model selection preserves the address.

The endpoint page offers Find models and Use an API key; the latter requests masked authorization while preserving the entered address. Endpoint text never enters the composer.

Provider-owned cloud forms collect ordered, required, non-secret coordinates without turning them into search text or composer input. Intermediate fields advance one at a time; the last field offers discovery with the composed credential or explicit masked credential entry. Amazon Bedrock currently contributes its region form. Google Vertex, Azure and custom-server setup remain outside the reachable UI until their phases are complete.

Integration keeps exactly three welcome choices. Cloud and API profiles share Select a provider; cloud PTY checks reach their forms through provider search.
