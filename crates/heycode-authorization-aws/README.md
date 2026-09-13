# heycode-authorization-aws

Provider-owned AWS authentication contributions: the explicit Amazon Bedrock
API key, the AWS SDK credential chain, and the safe status report over both.

PAWS01 answers one question — *what AWS authority does this machine have, and
did anything actually prove it* — and answers it without publishing a single
byte of credential material.

## The two credential paths

**Explicit Bedrock API key.** Plugin `authorization-aws` owns
`authorization_flow:aws-bedrock-api-key` and registers it as a Context-owned
effect, so shutdown or a failed activation removes exactly that row. The flow
reuses the shared S09 masked-entry flow; `AwsBedrockApiKeyValidator` sends the
documented `Authorization: Bearer <key>` to `GET /foundation-models` on the
regional control plane `https://bedrock.{region}.amazonaws.com`. That is the
cheapest documented request that proves the key is accepted.

**AWS SDK credential chain.** Discovery reads the documented environment
variables and the shared `config`/`credentials` files and reports *which*
source the chain resolves to, in this order:

1. static environment keys (`AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY`);
2. web identity (`AWS_WEB_IDENTITY_TOKEN_FILE` + `AWS_ROLE_ARN`);
3. the effective profile's shared configuration — static keys, `role_arn`,
   `sso_session` / legacy `sso_start_url`, `credential_process`;
4. the container credential endpoint
   (`AWS_CONTAINER_CREDENTIALS_RELATIVE_URI`, else
   `AWS_CONTAINER_CREDENTIALS_FULL_URI`).

SDKs differ in the details of their own ordering. This is a diagnostic over
the same documented sources, not a re-implementation of an SDK resolver.

## Four outcomes, and why none of them widens

`AwsCredentialStatus` keeps `Absent`, `Undetermined`, `Valid` and `Rejected`
apart, and `is_valid()` is true only for `Valid`. "The check could not reach
AWS" is a different fact from "AWS said no", which is a different fact again
from "you have no credentials" — and the middle one is the one a naive
implementation quietly turns into either of the others (GOTCHAS #41/#45).

The same discipline runs through the smaller resolutions. An unreadable
`~/.aws/config` makes the region `Undetermined`, never `Unresolved`: nothing
established that no region is set. A malformed `AWS_PROFILE` is `Malformed`,
never silently the `default` profile, because the SDKs would fail on the same
value. A malformed region reports its *origin* and resolves to nothing; no
region is ever guessed, because sending a caller's key to `us-east-1` because
nothing said otherwise puts an account boundary on the other side of a guess.

## What "without secrets" means here

Provenance is reported as **names**: an environment variable name, a profile
name, a heycode credential reference. That is the line, and it is drawn in the
type system — no variant of `AwsCredentialSource`, `AwsUndetermined`,
`AwsRejection` or `AwsCredentialStatus` has a field a value could live in, so
a secret, an account id, a role ARN or an IAM Identity Center start URL has
nowhere to go. `AwsRegionResolution::Malformed` names its origin and not the
rejected text for the same reason.

Two supporting rules keep files and responses from becoming the leak:

- The shared-file reader is private and offers two operations — *which
  settings does this profile define* and *what is the value of one named safe
  setting*. `aws_secret_access_key` is observed as a key name; its value is
  never lifted into a returned structure.
- The container credential endpoint returns real AWS secret material. Its
  deserializer reduces each credential field to "was it non-empty" at parse
  time and keeps no value, and the response is dropped by the caller. It is
  not zeroized — the bytes arrive inside the shared HTTP response buffer,
  which this crate does not own.

The container token itself is carried as a `CredentialSecret`, so it is
redacted in `Debug` and exposed only at the header it is written into.

## Endpoint safety

`AWS_CONTAINER_CREDENTIALS_FULL_URI` is honoured only over HTTPS or to a
loopback/link-local host (`localhost`, `127.0.0.0/8`, `[::1]`,
`169.254.0.0/16`, `[fd00:ec2::23]`). Anything else is
`Rejected { UnsafeEndpoint }` and no request is made, because the container
authorization token would otherwise travel in plaintext to whatever host the
environment named.

A named token source that yields nothing — an absent, unreadable or empty
`AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE` — is `Rejected`
`{ IncompleteConfiguration }` before the request, not an unauthenticated
attempt. Sending it anyway would collect a 403 and report the role as
rejected, which blames AWS for a local mistake.

## Known limits

- **No SigV4 signer.** Static keys, an assumed role, an SSO session, a
  `credential_process` helper and a web identity can only be proven by a
  signed `sts:GetCallerIdentity`, and this crate composes no signer and adds
  no AWS SDK dependency. Those sources are reported
  `Undetermined { RequiresSignedRequest }` — discovered, located, unproven.
  The container endpoint is the one chain source with a documented unsigned
  endpoint, and it is validated for real.
- **No IMDS probe.** IMDSv2 needs a `PUT` the shared HTTP boundary does not
  offer. An EC2 host with only an instance profile therefore reports `Absent`,
  which means "no source this crate can inspect" — not "the instance metadata
  service holds nothing".
- **A 403 is reported as a rejection.** The Bedrock control plane returns 403
  both for an unrecognised key and for a valid key without
  `bedrock:ListFoundationModels`. The closed S09 taxonomy maps both to
  `unauthorized`; distinguishing them would mean interpreting an error-type
  header this crate has no live evidence for.
- **INI parsing is minimal.** Comments must be on their own line, and indented
  AWS sub-properties are skipped rather than parsed as nested settings.

## Composition

The plugin injects `authorization`, `credentials` and `http`, contributes the
flow, and publishes `AwsAuthService` under service key `aws-auth` — the
effective profile, region and both path statuses that PAWS02/PAWS03 need to
build a request. `AwsHost` is the single boundary for ambient host facts
(environment, configuration files, home directory), so discovery is
deterministic under test and no production code path reaches for process state
on its own. All ordinary tests use injected hosts and transports.

The one exception is the explicitly gated hosted canary
`live_bedrock_api_key_validation_smoke`. It runs only with `HEYCODE_E2E=1`, a
non-empty process-scoped `AWS_BEARER_TOKEN_BEDROCK`, and a region resolved by
the production AWS profile/region rules. The key moves directly into
`CredentialSecret`; output carries only the validator's closed failure class.
An absent prerequisite skips, and no run on this host has supplied the full
set, so the canary's existence is not live evidence.

An explicit persisted connection region can override host defaults for catalog consumers, API-key validation and status. Reports attribute it to the connection, and fixture coverage verifies the actual validation request uses that same region without changing process environment.

When Bedrock is the effective saved route, the composition root also supplies the exact saved credential reference to this authorization flow. Catalog admission and inference receive that same query; a custom reference therefore cannot validate one record and dispatch with a provider-default alias.
