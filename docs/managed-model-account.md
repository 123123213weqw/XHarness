# Account model service retirement

XHarness uses models that you configure under **Settings → Models**, on the
Host where the chat runs. No XHarness sign-in is needed. The client no longer
ships device sign-in, an account menu, or first-party model credits.

The old managed route and token reference are inactive, even if still present
in a Host's saved settings or native credential store. The updated Host does
not read that credential or configure a route that references it. The
upgrade does not erase settings or credentials, revoke an external account,
change GitHub login, or alter the authentication required to connect to a Host.

If a chat was pinned to a retired account model, its history is still
available. Configure a provider with its own key, then explicitly select a
model in the chat before sending. Existing histories and in-flight work on
unupdated Hosts are unchanged; no requests are silently rerouted.
