# AWS client

## Authentication choices and prior art

| Client | Documented ways to access AWS resources | Lesson for this client |
| --- | --- | --- |
| [AWS Toolkit for VS Code](https://docs.aws.amazon.com/toolkit-for-vscode/latest/userguide/connect.html) | Detects existing AWS CLI credentials; offers IAM Identity Center through a browser and an IAM access-key profile. | Keep organization sign-in separate from existing AWS credentials. |
| [AWS Toolkit for JetBrains](https://docs.aws.amazon.com/toolkit-for-jetbrains/latest/userguide/account-connect.html) and [Visual Studio](https://docs.aws.amazon.com/toolkit-for-visual-studio/latest/user-guide/connect.html) | Offer a browser-based IAM Identity Center flow and IAM credentials; also recognize existing credentials. | A browser button for Identity Center still needs the organization's Start URL and region. |
| [Cyberduck for S3](https://docs.cyberduck.io/protocols/s3/) | Offers direct IAM Identity Center sign-in, access keys, AWS CLI profiles, and temporary STS credentials. | A desktop client can expose several paths without treating them as one login. |
| [AWS CLI Console login](https://docs.aws.amazon.com/cli/latest/userguide/cli-configure-sign-in.html) | `aws login` opens browser authentication for existing Console identities and manages temporary credentials. It is distinct from IAM Identity Center. | Offer this as the simplest default for Console users while showing its CLI version and IAM policy requirements. |

Builder ID sign-in in the Toolkits is documented for CodeCatalyst or Amazon Q;
it does not generally authorize access to an existing account's SQS resources.

## Using the SQS client

Run the standalone SQS window from this workspace:

```bash
cargo run -p aws
```

The default **AWS Console** choice opens a browser sign-in through `aws login`.
Enter the SQS region, then click **Open AWS in browser**. This route requires
AWS CLI 2.32 or later. IAM users and federated roles also need the
`SignInLocalDevelopmentAccess` policy. The CLI stores its renewable, temporary
credentials in an app-owned directory under `~/.ginka/aws-console` (or
`GINKA_HOME/aws-console`), separate from personal AWS profiles. An explicit
sign-in replaces the prior Console session in that directory so another account
can be chosen. The app invokes SQS through that isolated CLI profile. The CLI's
Console session and permission policies determine how long it remains usable.

**Connection settings and other sign-in options** offers **IAM Identity Center**
for organizations with a Start URL. Enter that URL, the Identity Center region,
and the SQS region, then use the browser PKCE flow. The app receives the result
through a local callback. **Device code** approves sign-in on another device.
After approval, choose an assigned AWS account and role (a sole choice is
selected automatically). This route signs SQS requests directly with temporary
role credentials and needs neither AWS CLI nor an AWS profile.

After a successful sign-in, the selected method and non-secret connection
fields are saved to `~/.ginka/aws.json` (or under `GINKA_HOME`). On later runs
they are restored. The window uses `AWS_SSO_START_URL`, `AWS_SSO_REGION`, and
`AWS_REGION` (or `AWS_DEFAULT_REGION`) when set, overriding saved values. The
native Identity Center route keeps tokens and role credentials only in memory;
a restart requires a new browser sign-in. Select a
queue to see approximate available, in-flight and delayed counts, send a
message, receive up to ten messages per request, delete a received message, or make it
available again immediately. Create a standard queue by name, or a FIFO queue
with a `.fifo` suffix; new FIFO queues use content-based deduplication. Filter
the queue list by name or URL, or paste a queue URL and open it directly when the
identity has queue access but cannot list queues. Changing the login or region
clears the previous queue and message data; refresh the list or open a URL for
the new account, role, or region. FIFO queues also require a message group ID. Sending a
message can be delayed by 0–900 seconds on standard queues; leave **Send delay**
blank to use the queue default. Add up to ten String message attributes as a JSON object, such as
`{"orderId":"42"}`. Leave the attribute field blank to send none. Received
messages show returned String and Number values and base64-encoded Binary values.
The detail pane shows the queue delay default and lets you set **Queue delay**
to 0–900 seconds for either queue type. FIFO queues only
support queue-level delay; changing it also affects messages already queued.
The detail pane also shows and sets the queue's default **Visibility timeout**
from 0–43,200 seconds and default **Receive wait** from 0–20 seconds. SQS may
take up to 60 seconds to apply these settings.
The **Message retention** setting accepts 60–1,209,600 seconds (one minute to
14 days) and requires a second confirmation click. Shortening it can expire
existing messages older than the new period; SQS may take up to 15 minutes to
apply the change.
Receiving a message applies the queue's visibility timeout even if you do not delete it.
The optional visibility field beside **Receive** overrides that timeout for one
receive, from 0 to 43,200 seconds; leave it blank to use the queue default.
Zero makes a received message immediately available for another receive. The
receive control cycles through a short poll (the initial choice), a 20-second
long poll, and the queue's default receive wait. Choosing the queue default
omits the per-request wait setting, so changes to the queue setting apply to
later receives. **Max messages** chooses a per-request limit of 1–10, initially
10. A receive may return fewer messages than requested.
Each received row shows the approximate receive count and original sent time
when SQS returns them. Sent time is shown in the local time zone. FIFO messages
also show their message group ID. These values describe the message; deleting or
changing visibility still uses the receipt handle from the current receive.
The **Make available again** action sets that message's visibility timeout to
zero so it can be received again. To change one received message's timeout,
enter 0–43,200 seconds in **Change message visibility** and use **Apply
visibility** on its row. The new period starts when the change is applied and
does not alter the queue default; SQS may reject a value beyond the time left
for that message. The detail pane also purges every message or
deletes the selected queue after a second confirmation click. Purging includes
in-flight messages and may take up to 60 seconds; messages sent during that
interval can also be removed.

The selected AWS identity needs `sqs:ListQueues` to browse and
`sqs:CreateQueue` to create queues. Queue operations need
`sqs:GetQueueAttributes`, `sqs:SetQueueAttributes`, `sqs:SendMessage`, `sqs:ReceiveMessage`,
`sqs:DeleteMessage`, `sqs:ChangeMessageVisibility`, `sqs:PurgeQueue` and
`sqs:DeleteQueue` as appropriate. Opening
a URL directly does not call `ListQueues`. Native Identity Center credentials
expire and a fresh sign-in is currently required after expiry. Console login
uses the CLI's isolated credential cache and refresh behavior.

The login choices follow the documented behavior of [AWS Toolkit for VS Code](https://docs.aws.amazon.com/toolkit-for-vscode/latest/userguide/connect.html),
[AWS Toolkit for JetBrains](https://docs.aws.amazon.com/toolkit-for-jetbrains/latest/userguide/account-connect.html),
and [Cyberduck's S3 connection](https://docs.cyberduck.io/protocols/s3/):
desktop clients offer organization Identity Center or existing AWS credentials.
The browser-based Console route uses [AWS CLI Console credentials](https://docs.aws.amazon.com/cli/latest/userguide/cli-configure-sign-in.html).
[AWS Builder ID](https://docs.aws.amazon.com/signin/latest/userguide/differences-builder-id.html)
is not the standard way to access SQS in an existing AWS account. AWS documents
a limited new-account experience separately from the usual IAM authorization.
