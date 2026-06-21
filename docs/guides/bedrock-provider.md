# AWS Bedrock provider

FlowForge can talk to Anthropic (and other) models hosted on **Amazon Bedrock**.
Bedrock is configured in **Settings → Model** as a provider card alongside the
local backends (candle-vLLM, Ollama).

All secret material — the IAM secret access key, the session token, and the
Bedrock bearer API key — is stored in your **OS keychain** (macOS Keychain /
Windows Credential Manager). It is never written to the app's config file, never
committed to disk in plaintext, and never read back over the UI. The only signal
the UI keeps is whether a key is *set*.

> Linux secret storage (libsecret/D-Bus) ships separately — macOS and Windows
> first. See #202.

## Add the provider

1. Open **Settings → Model**.
2. If no Bedrock card is present, click **Add provider → AWS Bedrock**.
3. Expand the **AWS Bedrock** card.

## Region

Set the AWS **Region** for your Bedrock access (e.g. `us-east-1`, `eu-central-1`).
The backend derives the runtime endpoint from it:
`bedrock-runtime.<region>.amazonaws.com`. The field offers common regions as
suggestions but accepts any region string.

## Authentication

Pick one of three modes with the **Authentication** toggle:

### Profile (recommended for local dev)

Uses a named profile from `~/.aws/config` / `~/.aws/credentials` (the same
profiles the AWS CLI uses). Enter the **AWS Profile** name, or leave it blank for
the `default` profile. No secret is stored by FlowForge in this mode — credentials
resolve through the AWS SDK's standard chain.

```ini
# ~/.aws/config
[profile bedrock-profile]
region = us-east-1
# ... SSO or source_profile config ...
```

### IAM Keys

Long-lived (or temporary) IAM credentials entered directly:

- **Access Key ID** — the `AKIA…` identifier (non-secret; stored with the
  connection).
- **Secret Access Key** — write-only; stored in the keychain.
- **Session Token** *(optional)* — write-only; required only for temporary
  (STS) credentials.

The IAM principal needs `bedrock:InvokeModel` /
`bedrock:InvokeModelWithResponseStream` on the models you use, and
`bedrock:ListInferenceProfiles` if you want model discovery (see below).

### API Key

A Bedrock **bearer API key** (`br-…`), write-only and stored in the keychain.
Note: a bearer key may be able to converse without having
`bedrock:ListInferenceProfiles`, in which case model discovery returns nothing
and you enter the model id by hand.

After entering credentials, click **Save**. Use **Test Connection** to verify the
active mode end-to-end — it runs a minimal probe against the configured model and
reports success or the underlying error.

## Models

Bedrock model ids on FlowForge are **inference-profile ids**, e.g.:

- `us.anthropic.claude-opus-4-0` (cross-region)
- `us.anthropic.claude-sonnet-4-0` (cross-region)
- `amazon.nova-pro-v1:0` (on-demand)

In the Bedrock card's **Models** section, click **Add model** to discover
available profiles via `ListInferenceProfiles`, then select one. The radio marks
the **default model** — the model used when no session or phenotype specifies
one. If discovery is unavailable (e.g. an API key without the list permission),
the connection still works once you've set a default model id.
