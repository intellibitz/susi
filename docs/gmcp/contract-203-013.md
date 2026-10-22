# GMCP-203-013 contract: MRTR, elicitation, sampling and roots interaction model

This contract binds every normative clause of vector VC-203-013. Each row in the behavior table is normative; its identifier, level, clause and production path are pinned.

## Normative clause

The MVP must implement the 198 pinned clauses below across the GMCP interaction model: input_required, typed inputRequests, retry with inputResponses, nested elicitation, sampling, roots, cancellation, deadlines, progress, partial failure and explicit handle ownership. Every human or model decision is attributable, policy-checked and resumable.

## Pinned sources

| repository | document | sha256 | lines |
|---|---|---|---|
| modelcontextprotocol | docs/specification/2025-11-25/client/elicitation.mdx | a60c4955e9fbacd5b21c856f7a8f1ccdaad91dc6c600347762482fd2750e6a3f | 30-781 |
| modelcontextprotocol | docs/specification/2025-11-25/client/roots.mdx | 5ac98aa829b9c719ed2600fd5cd0d764833054d97ce7221d1660ccbcf06a5478 | 27-184 |
| modelcontextprotocol | docs/specification/2025-11-25/client/sampling.mdx | 4983c3deda69d135e7e756f9e51d62fb1174b0ad6176eed5a40d0f12bfaf0d15 | 25-635 |
| modelcontextprotocol | docs/specification/2026-07-28/basic/patterns/mrtr.mdx | a8671eb2a0d292c0dcbae19666ff4fc3d97f294ce8107e9f38259cc5a02a066b | 10-272 |
| modelcontextprotocol | docs/specification/2026-07-28/client/elicitation.mdx | 74351d1081681695536e59a55588a67ed66a76db6057846c263ce2f465fab4c2 | 30-671 |
| modelcontextprotocol | docs/specification/2026-07-28/client/roots.mdx | 016aea34d76ccce86a82509dbf2699833bb8547bde5b3f4a8eb086dfd26a9ba2 | 13-156 |
| modelcontextprotocol | docs/specification/2026-07-28/client/sampling.mdx | 32fe2bb69f5a7caa85a6b9501f465cb1fb888d0c669cd13b23a071071a029df1 | 13-678 |

## Canonical data model

Requests carry a typed `inputRequests` map keyed by handle; responses carry `inputResponses` for the same handles. Elicitation, sampling and roots are explicit sub-flows with their own handles and deadlines. Cancellation, deadlines and progress are properties of the request, not of a transport.

## Behavior table

| id | level | clause | production path |
|---|---|---|---|
| MCP-2025-1125-client-elicitation-mdx-0030 | must_not | - Servers **MUST NOT** use form mode elicitation to request sensitive information such as | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0032 | must | - Servers **MUST** use [URL mode](#url-mode-elicitation-requests) for interactions involving | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0040 | must | MCP clients **MUST**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0051 | must | Clients that support elicitation **MUST** declare the `elicitation` capability during | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0075 | must | Clients declaring the `elicitation` capability **MUST** support at least one mode (`form` or `url`). | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0077 | must_not | Servers **MUST NOT** send elicitation requests with modes that are not supported by the client. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0085 | must | All elicitation requests **MUST** include the following parameters: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0097 | must | For backwards compatibility, servers **MAY** omit the `mode` field for form mode elicitation requests. Clients **MUST** treat requests without a `mode` field as form mode. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0103 | must | Form mode elicitation requests **MUST** either specify `mode: "form"` or omit the `mode` field, and include these additional parameters: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0233 | should | All primitive types support optional default values to provide sensible starting points. Clients that support defaults SHOULD pre-populate form fields with these values. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0340 | must | URL mode elicitation requests **MUST** specify `mode: "url"`, a `message`, and include these additional parameters: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0347 | must | The `url` parameter **MUST** contain a valid URL. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0398 | may | Servers **MAY** send a `notifications/elicitation/complete` notification when an | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0403 | must | - **MUST** only send the notification to the client that initiated the elicitation request. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0404 | must | - **MUST** include the `elicitationId` established in the original `elicitation/create` request. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0408 | must | - **MUST** ignore notifications referencing unknown or already-completed IDs. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0409 | may | - **MAY** wait for this notification to automatically retry requests that received a [URLElicitationRequiredError](#error-handling), update the user interface, or otherwise continue an interaction. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0410 | should | - **SHOULD** still provide manual controls that let the user retry or cancel the original request (or otherwise resume interacting with the client) if the notification never arrives. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0426 | must_not | When a request cannot be processed until an elicitation is completed, the server **MAY** return a [`URLElicitationRequiredError`](#error-handling) (code `-32042`) to indicate to the client that a URL mode elicitation is required. The server | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0428 | must | The error **MUST** include a list of elicitations that are required to complete before the original can be retried. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0430 | must | Any elicitations returned in the error **MUST** be URL mode elicitations and have an `elicitationId` property. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0578 | must | Servers implementing elicitation **MUST** securely associate this state with individual users following the guidelines in the [security best practices](../basic/security_best_practices) document. Specifically: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0580 | must_not | - State **MUST NOT** be associated with session IDs alone | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0581 | must | - State storage **MUST** be protected against unauthorized access | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0582 | must | - For remote MCP servers, user identification **MUST** be derived from credentials acquired via [MCP authorization](../basic/authorization) when possible (e.g. `sub` claim) | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0607 | must_not | Authorization with external APIs enabled by URL mode elicitation is separate from [MCP authorization](../basic/authorization). MCP servers **MUST NOT** rely on URL mode elicitation to authorize users for themselves. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0627 | must_not | 1. **The third-party credentials MUST NOT transit through the MCP client**: The client must never see third-party credentials to protect the security boundary | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0628 | must_not | 2. **The MCP server MUST NOT use the client's credentials for the third-party service**: That would be [token passthrough](../basic/security_best_practices#token-passthrough), which is forbidden | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0629 | must | 3. **The user MUST authorize the MCP server directly**: The interaction happens outside the MCP protocol, without involving the MCP client | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0632 | must_not | Credentials obtained via URL mode elicitation are distinct from the MCP server credentials used by the MCP client. The MCP server **MUST NOT** transmit credentials obtained through URL mode elicitation to the MCP client. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0694 | must | Servers **MUST** return standard JSON-RPC errors for common failure cases: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0698 | must | Clients **MUST** return standard JSON-RPC errors for common failure cases: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0704 | must | 1. Servers **MUST** bind elicitation requests to the client and user identity | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0705 | must | 1. Clients **MUST** provide clear indication of which server is requesting information | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0706 | should | 1. Clients **SHOULD** implement user approval controls | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0707 | should | 1. Clients **SHOULD** allow users to decline elicitation requests at any time | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0708 | should | 1. Clients **SHOULD** implement rate limiting | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0709 | should | 1. Clients **SHOULD** present elicitation requests in a way that makes it clear what information is being requested and why | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0715 | must_not | 1. **MUST NOT** include sensitive information about the end-user, including credentials, personal identifiable information, etc., in the URL sent to the client in a URL elicitation request. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0716 | must_not | 2. **MUST NOT** provide a URL which is pre-authenticated to access a protected resource, as the URL could be used to impersonate the user by a malicious client. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0717 | should_not | 3. **SHOULD NOT** include URLs intended to be clickable in any field of a form mode elicitation request. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0718 | should | 4. **SHOULD** use HTTPS URLs for non-development environments. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0722 | must | Clients implementing URL mode elicitation **MUST** handle URLs carefully to prevent users from unknowingly clicking malicious links. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0726 | must_not | 1. **MUST NOT** automatically pre-fetch the URL or any of its metadata. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0727 | must_not | 2. **MUST NOT** open the URL without explicit consent from the user. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0728 | must | 3. **MUST** show the full URL to the user for examination before consent. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0729 | must | 4. **MUST** open the URL provided by the server in a secure manner that does not enable the client or LLM to inspect the content or user inputs. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0731 | should | 5. **SHOULD** highlight the domain of the URL to mitigate subdomain spoofing. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0732 | should | 6. **SHOULD** have warnings for ambiguous/suspicious URIs (i.e., containing Punycode). | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0733 | should_not | 7. **SHOULD NOT** render URLs as clickable in any field of an elicitation request, except for the `url` field in a URL elicitation request (with the restrictions detailed above). | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0737 | must_not | Servers **MUST NOT** rely on client-provided user identification without server verification, as this can be forged. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0738 | should | Instead, servers **SHOULD** follow [security best practices](../basic/security_best_practices). | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0747 | must_not | 1. Servers **MUST NOT** request sensitive information (passwords, API keys, etc.) via form mode | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0748 | should | 2. Clients **SHOULD** validate all responses against the provided schema | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0749 | should | 3. Servers **SHOULD** validate received data matches the requested schema | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0753 | must | URL mode elicitation returns a URL that an attacker can use to send to a victim. The MCP Server **MUST** verify the identity of the user who opens the URL before accepting information. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0767 | must | To prevent this attack, the server **MUST** ensure that the user who started the elicitation request (the end-user who is accessing the server via the MCP client) is the same user who completes the authorization flow. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-elicitation-mdx-0781 | must | In all implementations, the server **MUST** ensure that the mechanism to determine the user's identity is resilient to attacks where an attacker can modify the elicitation URL. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-roots-mdx-0027 | must | Clients that support roots **MUST** declare the `roots` capability during | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-roots-mdx-0078 | must | When roots change, clients that support `listChanged` **MUST** send a notification: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-roots-mdx-0110 | must | - `uri`: Unique identifier for the root. This **MUST** be a `file://` URI in the current | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-roots-mdx-0142 | should | Clients **SHOULD** return standard JSON-RPC errors for common failure cases: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-roots-mdx-0165 | must | 1. Clients **MUST**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-roots-mdx-0171 | should | 2. Servers **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-roots-mdx-0178 | should | 1. Clients **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-roots-mdx-0184 | should | 2. Servers **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0025 | should | For trust & safety and security, there **SHOULD** always | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0028 | should | Applications **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0040 | must_not | Clients **MUST** declare support for tool use via the `sampling.tools` capability to receive tool-enabled sampling requests. Servers **MUST NOT** send tool-enabled sampling requests to Clients that have not declared support for tool use via | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0044 | must | Clients that support sampling **MUST** declare the `sampling` capability during | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0083 | should | soft-deprecated. Servers **SHOULD** avoid using these values (e.g. can just | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0084 | should_not | omit `includeContext` since it defaults to `"none"`), and **SHOULD NOT** use | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0372 | must | When a user message contains tool results (type: "tool_result"), it **MUST** contain ONLY tool results. Mixing tool results with other content types (text, image, audio) in the same message is not allowed. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0430 | must | When using tool use in sampling, every assistant message containing `ToolUseContent` blocks **MUST** be followed by a user message that consists entirely of `ToolResultContent` blocks, with each tool use (e.g. with `id: $id`) matched by a c | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0469 | must | - `{mode: "required"}`: Model MUST use at least one tool before completing | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0470 | must_not | - `{mode: "none"}`: Model MUST NOT use any tools | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0480 | may | Implementations wrapping providers that support disabling parallel tool use MAY expose this as an extension, but it is not part of the core MCP specification. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0571 | may | - Clients **MAY** map hints to equivalent models from different providers | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0594 | should | Clients **SHOULD** return errors for common failure cases: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0626 | should | 1. Clients **SHOULD** implement user approval controls | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0627 | should | 2. Both parties **SHOULD** validate message content | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0628 | should | 3. Clients **SHOULD** respect model preference hints | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0629 | should | 4. Clients **SHOULD** implement rate limiting | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0630 | must | 5. Both parties **MUST** handle sensitive data appropriately | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0634 | must | 6. Servers **MUST** ensure that when replying to a `stopReason: "toolUse"`, each `ToolUseContent` item is responded to with a `ToolResultContent` item with a matching `toolUseId`, and that the user message contains only tool results (no oth | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-client-sampling-mdx-0635 | should | 7. Both parties **SHOULD** implement iteration limits for tool loops | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0010 | must | requests. Servers **MUST** send server-to-client requests (such as | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0020 | must | `io.modelcontextprotocol/clientCapabilities`). Every request **MUST** include | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0130 | must_not | - `requestState` _(optional)_: An opaque string meaningful only to the server. Clients **MUST NOT** inspect, parse, modify, or make any assumptions about its contents. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0184 | may | Servers **MAY** send `InputRequiredResult` responses on the following client requests: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0192 | must_not | Servers **MUST NOT** send `InputRequiredResult` responses on any other client requests. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0226 | may | 1. Servers **MAY** respond to any [supported client request](#supported-requests) with an `InputRequiredResult`. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0227 | may | 1. The `InputRequiredResult` **MAY** include an `inputRequests` field. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0228 | must | - `inputRequests` keys are server assigned identifiers and **MUST** be unique within the scope of the request. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0229 | must | - `inputRequests` values are request objects that **MUST** be one of [`ElicitRequest`](/specification/2026-07-28/schema#elicitrequest), [`CreateMessageRequest`](/specification/2026-07-28/schema#createmessagerequest), or [`ListRootsRequest`] | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0231 | may | 1. The `InputRequiredResult` **MAY** include a `requestState` field. If specified, this field is an opaque string meaningful only to the server. Servers are free to encode the state in any format (e.g. base64-encoded JSON, encrypted JWT, se | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0232 | must | 1. If a client request contains a `requestState` field, servers **MUST** treat `requestState` as an attacker-controlled input. If `requestState` influences authorization, resource access, or business logic, servers **MUST** protect its inte | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0233 | must | and **MUST** reject state that fails verification. Integrity protection **MAY** be omitted only when tampering can cause nothing worse than request failure. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0234 | should | 1. To prevent replay, servers **SHOULD** include the following inside the integrity-protected `requestState` payload and verify each on receipt: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0242 | must | (e.g., one-time redemptions) **MUST** enforce that invariant server-side. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0245 | must | 1. Servers **MUST** include at least one of `inputRequests` or `requestState` in every `InputRequiredResult` response. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0246 | must_not | 1. Servers **MUST NOT** send an `inputRequests` that the client has not declared support for in its capabilities. For example, if a client does not declare support for `elicitation`, the server **MUST NOT** include any `elicitation/create`  | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0247 | must_not | 1. Servers **MUST NOT** assume that clients will fulfill the `inputRequests` or retry the original request. Servers **MAY** choose to return an `InputRequiredResult` on multiple attempts at the same request if they want to repeatedly prompt | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0251 | must | 1. If a client receives an `InputRequiredResult` that contains the `inputRequests` field, the client **MUST** construct the requested | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0253 | may | the client **MAY** retry the original request immediately. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0254 | must | 1. If an `InputRequiredResult` contains the `requestState` field, the client **MUST** echo back the exact value of that field when retrying the original request. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0255 | must_not | Clients **MUST NOT** inspect, parse, modify, or make any assumptions about the `requestState` contents. If the `InputRequiredResult` does not contain a `requestState` field, the client **MUST NOT** include one in the retry. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0256 | must | 1. The JSON-RPC `id` **MUST** be different between the initial request and the retry, as they are independent requests. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0257 | must_not | 1. Both the `inputRequests` and `requestState` fields affect only the client's retry of the original request. They **MUST NOT** be used for any other request that the client may be sending in parallel. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0261 | should | Servers **SHOULD** validate that the data provided by the client is a valid `InputResponses` object and that the information inside can be correctly parsed. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0262 | should | Protocol errors (malformed JSON, invalid schema, internal server errors) **SHOULD** return a JSON-RPC error response with an appropriate error code and message. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0264 | should | If additional, unexpected parameters are provided in the `InputResponses` object, the server **SHOULD** ignore any information it does not recognize or need. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0267 | should | the server **SHOULD** respond with a new `InputRequiredResult` requesting the missing information again, rather than returning an error. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-mrtr-mdx-0272 | must | bypass authorization checks, or corrupt server logic. Servers **MUST** validate request state as described in the [server requirements](#server-requirements-basic-workflow) above. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0030 | must_not | - Servers **MUST NOT** use form mode elicitation to request sensitive information such as | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0032 | must | - Servers **MUST** use [URL mode](#url-mode-elicitation-requests) for interactions involving | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0040 | must | MCP clients **MUST**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0051 | must | Clients that support elicitation **MUST** declare the `elicitation` capability in | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0079 | must | Clients declaring the `elicitation` capability **MUST** support at least one mode (`form` or `url`). | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0081 | must_not | Servers **MUST NOT** send elicitation requests with modes that are not supported by the client. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0087 | may | Servers **MAY** request information from a user during the processing of a client request, by sending an [`InputRequiredResult`](/specification/2026-07-28/basic/patterns/mrtr#inputrequiredresult) | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0090 | must | All elicitation requests **MUST** include the following parameters: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0102 | must | For backwards compatibility, servers **MAY** omit the `mode` field for form mode elicitation requests. Clients **MUST** treat requests without a `mode` field as form mode. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0108 | must | Form mode elicitation requests **MUST** either specify `mode: "form"` or omit the `mode` field, and include these additional parameters: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0237 | should | All primitive types support optional default values to provide sensible starting points. Clients that support defaults SHOULD pre-populate form fields with these values. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0332 | must | URL mode elicitation requests **MUST** specify `mode: "url"`, a `message`, and include these additional parameters: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0338 | must | The `url` parameter **MUST** contain a valid URL. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0381 | should | final result or responds with another `InputRequiredResult`. Clients **SHOULD** provide | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0475 | must | However, if state is stored, servers implementing elicitation **MUST** securely associate this state with individual users following the guidelines in the [security best practices](/docs/2026-07-28/tutorials/security/security_best_practices | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0477 | must | - State storage **MUST** be protected against unauthorized access | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0478 | must | - For remote MCP servers, user identification **MUST** be derived from credentials acquired via [MCP authorization](../basic/authorization) when possible (e.g. `sub` claim) | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0503 | must_not | Authorization with external APIs enabled by URL mode elicitation is separate from [MCP authorization](../basic/authorization). MCP servers **MUST NOT** rely on URL mode elicitation to authorize users for themselves. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0523 | must_not | 1. **The third-party credentials MUST NOT transit through the MCP client**: The client must never see third-party credentials to protect the security boundary | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0524 | must_not | 2. **The MCP server MUST NOT use the client's credentials for the third-party service**: That would be [token passthrough](/docs/2026-07-28/tutorials/security/security_best_practices#token-passthrough), which is forbidden | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0525 | must | 3. **The user MUST authorize the MCP server directly**: The interaction happens outside the MCP protocol, without involving the MCP client | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0528 | must_not | Credentials obtained via URL mode elicitation are distinct from the MCP server credentials used by the MCP client. The MCP server **MUST NOT** transmit credentials obtained through URL mode elicitation to the MCP client. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0591 | must | Servers **SHOULD NOT** assume that elicitation requests will always succeed, and **MUST** handle cases where the user declines or cancels the elicitation, or where the client fails to process the request. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0595 | must | 1. Servers **MUST** bind elicitation requests to the client and user identity | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0596 | must | 1. Clients **MUST** provide clear indication of which server is requesting information | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0597 | should | 1. Clients **SHOULD** implement user approval controls | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0598 | should | 1. Clients **SHOULD** allow users to decline elicitation requests at any time | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0599 | should | 1. Clients **SHOULD** present elicitation requests in a way that makes it clear what information is being requested and why | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0605 | must_not | 1. **MUST NOT** include sensitive information about the end-user, including credentials, personally identifiable information, etc., in the URL sent to the client in a URL elicitation request. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0606 | must_not | 2. **MUST NOT** provide a URL which is pre-authenticated to access a protected resource, as the URL could be used to impersonate the user by a malicious client. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0607 | should_not | 3. **SHOULD NOT** include URLs intended to be clickable in any field of a form mode elicitation request. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0608 | should | 4. **SHOULD** use HTTPS URLs for non-development environments. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0612 | must | Clients implementing URL mode elicitation **MUST** handle URLs carefully to prevent users from unknowingly clicking malicious links. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0616 | must_not | 1. **MUST NOT** automatically pre-fetch the URL or any of its metadata. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0617 | must_not | 2. **MUST NOT** open the URL without explicit consent from the user. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0618 | must | 3. **MUST** show the full URL to the user for examination before consent. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0619 | must | 4. **MUST** open the URL provided by the server in a secure manner that does not enable the client or LLM to inspect the content or user inputs. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0621 | should | 5. **SHOULD** highlight the domain of the URL to mitigate subdomain spoofing. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0622 | should | 6. **SHOULD** have warnings for ambiguous/suspicious URIs (i.e., containing Punycode). | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0623 | should_not | 7. **SHOULD NOT** render URLs as clickable in any field of an elicitation request, except for the `url` field in a URL elicitation request (with the restrictions detailed above). | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0627 | must_not | Servers **MUST NOT** rely on client-provided user identification without server verification, as this can be forged. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0628 | should | Instead, servers **SHOULD** follow [security best practices](/docs/2026-07-28/tutorials/security/security_best_practices). | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0637 | must_not | 1. Servers **MUST NOT** request sensitive information (passwords, API keys, etc.) via form mode | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0638 | should | 2. Clients **SHOULD** validate all responses against the provided schema | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0639 | should | 3. Servers **SHOULD** validate received data matches the requested schema | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0643 | must | URL mode elicitation returns a URL that an attacker can use to send to a victim. The MCP Server **MUST** verify the identity of the user who opens the URL before accepting information. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0657 | must | To prevent this attack, the server **MUST** ensure that the user who started the elicitation request (the end-user who is accessing the server via the MCP client) is the same user who completes the authorization flow. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-elicitation-mdx-0671 | must | In all implementations, the server **MUST** ensure that the mechanism to determine the user's identity is resilient to attacks where an attacker can modify the elicitation URL. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-roots-mdx-0013 | should_not | before it becomes eligible for removal. New implementations **SHOULD NOT** | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-roots-mdx-0014 | should | adopt it; existing implementations **SHOULD** migrate to passing directories | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-roots-mdx-0040 | must | Clients that support roots **MUST** declare the `roots` capability in | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-roots-mdx-0100 | must | - `uri`: Unique identifier for the root. This **MUST** be a `file://` URI in the current | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-roots-mdx-0137 | must | 1. Clients **MUST**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-roots-mdx-0143 | should | 2. Servers **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-roots-mdx-0150 | should | 1. Clients **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-roots-mdx-0156 | should | 2. Servers **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0013 | should_not | before it becomes eligible for removal. New implementations **SHOULD NOT** | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0014 | should | adopt it; existing implementations **SHOULD** migrate to integrating directly | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0037 | should | For trust & safety and security, there **SHOULD** always | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0040 | should | Applications **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0052 | must_not | Clients **MUST** declare support for tool use via the `sampling.tools` capability to receive tool-enabled sampling requests. Servers **MUST NOT** send tool-enabled sampling requests to Clients that have not declared support for tool use via | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0056 | must | Clients that support sampling **MUST** declare the `sampling` capability in | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0105 | should | **SHOULD** avoid using these values (e.g. can just omit `includeContext` since | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0106 | should_not | it defaults to `"none"`), and **SHOULD NOT** use them unless the client | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0380 | must | When a user message contains tool results (type: "tool_result"), it **MUST** contain ONLY tool results. Mixing tool results with other content types (text, image, audio) in the same message is not allowed. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0438 | must | When using tool use in sampling, every assistant message containing `ToolUseContent` blocks **MUST** be followed by a user message that consists entirely of `ToolResultContent` blocks, with each tool use (e.g. with `id: $id`) matched by a c | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0477 | must | - `{mode: "required"}`: Model MUST use at least one tool before completing | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0478 | must_not | - `{mode: "none"}`: Model MUST NOT use any tools | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0488 | may | Implementations wrapping providers that support disabling parallel tool use MAY expose this as an extension, but it is not part of the core MCP specification. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0523 | must | Sampling messages **MUST** contain a `role` field of `"user"` or `"assistant"`; and | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0526 | should_not | The list of messages in a sampling request **SHOULD NOT** be retained between | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0586 | may | - Clients **MAY** map hints to equivalent models from different providers | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0610 | may | The client **MAY** modify or ignore this field without communicating this to the server. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0624 | may | The client **MAY** modify or ignore this field without communicating this to the server. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0638 | must | The client **MUST** respect the `maxTokens` parameter. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0640 | may | The client **MAY** modify or ignore `temperature`, `stopSequences` and `metadata`. For | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0656 | may | - `stopReason`: The reason why sampling stopped, if known. The specification defines the following (non-exhaustive) stop reasons, although implementations **MAY** provide their own arbitrary values: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0669 | should | 1. Clients **SHOULD** implement user approval controls | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0670 | should | 2. Both parties **SHOULD** validate message content | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0671 | should | 3. Clients **SHOULD** respect model preference hints | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0672 | should | 4. Clients **SHOULD** implement rate limiting | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0673 | must | 5. Both parties **MUST** handle sensitive data appropriately | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0677 | must | 6. Servers **MUST** ensure that when replying to a `stopReason: "toolUse"`, each `ToolUseContent` item is responded to with a `ToolResultContent` item with a matching `toolUseId`, and that the user message contains only tool results (no oth | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-client-sampling-mdx-0678 | should | 7. Both parties **SHOULD** implement iteration limits for tool loops | crates/susi-gmcp/src/protocol.rs |

## Compatibility and deprecation decisions

Every clause carries an explicit disposition: adapter=82, modern=100, optional=16. A clause disposed as adapter is honored only through the explicit version adapter with downgrade telemetry and a removal policy.

## Threat model

A malicious peer can replay a handle, answer another principal's elicitation, or stall a request past its deadline. Handles are principal-scoped and single-use; responses are validated against the request that created them; deadlines and cancellation are enforced before any side effect.

## SLOs and resource budgets

Every interaction completes within its declared deadline or is cancelled with a typed error. Bounded queues and per-principal handle budgets prevent unbounded growth; progress is reported at least once per deadline window.

## Fixtures

Positive and negative fixtures for every row live in .agents/gmcp/evidence/VC-203-013.jsonl, keyed by row id; the tests in tests/gmcp_requirements.rs consume them.

## Evidence locations

.agents/gmcp/evidence/VC-203-013.jsonl holds the retained evidence; tests/gmcp_requirements.rs::gmcp_req_203_013_01_contract binds this document to the ledger.

## Ambiguities resolved

All clauses are resolved by this table: the ledger id is the canonical name, the production path is the only allowed implementation site, and the disposition decides whether the clause is honored on the modern path or only through the version adapter.
