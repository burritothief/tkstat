# Security policy

Security fixes are provided for the latest released version of tkstat.

Please report suspected vulnerabilities through GitHub's private vulnerability reporting for
`burritothief/tkstat`. Do not include credentials, private session logs, prompts, responses, or
other sensitive data in a public issue. If private reporting is unavailable, open a minimal public
issue requesting a private contact channel without disclosing exploit details.

`tkstat` reads local provider logs and stores normalized usage in a local SQLite database. Pricing
refresh is the only normal command that makes network requests; it fetches provider-owned pricing
documents and validates them before replacing last-known-good data.
