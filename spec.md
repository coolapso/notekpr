# Project Specification: Obsidian Meets Notion

**Version:** 0.2 — Initial Product Specification  
**Status:** Draft / Ready for technical design  
**License:** Open Source  
**Mandatory technology:** Rust

## 1. Vision

Build a self-hosted, local-first, collaborative knowledge management platform combining the best aspects of Obsidian and Notion.

The platform must provide:

- Obsidian's Markdown-first philosophy, local storage, offline capabilities, and interconnected knowledge.
- Notion's intuitive interface, collaboration, sharing, and multiuser experience.
- Secure, end-to-end encrypted synchronization.
- Native desktop and mobile applications.
- A headless synchronization server.
- Open APIs for integration with AI agents and external tools.

The application must be immediately usable without requiring users to write scripts, customize interfaces, or configure complex workflows.

**Core philosophy: Users own their data, control who can access it, and retain full functionality without depending on a proprietary cloud service.**

## 2. Technology Constraints

### Mandatory

**Rust must be the primary implementation language for the core platform**, including the synchronization engine, security-sensitive components, and server.

### Agent Discretion

The implementing agent is responsible for researching, evaluating, and selecting:

- Database and persistence technologies.
- Web frontend framework.
- Desktop and mobile application frameworks.
- Markdown editor.
- Real-time synchronization and collaboration protocols.
- CRDT or other conflict-resolution mechanisms.
- Cryptographic libraries and key-management implementation.
- API protocols.
- Local storage architecture.
- Search and indexing technologies.
- Deployment and infrastructure dependencies.

Prefer mature, actively maintained, open-source technologies.

Technology choices must be documented with their rationale, alternatives considered, security implications, and tradeoffs.

Avoid introducing unnecessary services, complex infrastructure, or vendor dependencies.

## 3. Core Architecture

The platform consists of three principal components.

### 3.1 Headless Synchronization Server

A standalone server responsible for:

- OIDC-based authentication.
- User identity mapping.
- Vault management.
- Access control and sharing.
- Synchronization coordination.
- Encrypted document and attachment storage.
- Revision history.
- Collaboration sessions.
- Device and session management.
- APIs for clients and integrations.

The server must not require the web frontend to operate.

**The server must never possess the keys required to decrypt user vault contents.**

The server should be lightweight, efficient, and suitable for self-hosting on modest infrastructure.

### 3.2 Web Frontend

A fully functional browser-based application providing:

- Markdown editing.
- Navigation and document organization.
- Multiple vaults.
- Private and shared workspaces.
- Live collaborative editing.
- Search and backlinks.
- Authentication through OIDC.
- Sharing and permission management.
- Revision history.
- Responsive mobile layout.

The web frontend must work without installing additional client software.

For security, cryptographic operations must happen client-side.

### 3.3 Native Applications

Provide native or native-packaged applications for:

- Linux.
- macOS.
- Windows.
- iOS.
- Android.

Native clients must provide:

- Local-first document access.
- Offline reading and editing.
- Local storage of synchronized vaults.
- Automatic synchronization.
- Secure OIDC authentication.
- Multiple vault support.
- Conflict handling.
- Synchronization status and error reporting.
- Secure management of local encryption keys.

Desktop clients must support access to real Markdown files, subject to the local encryption model.

Mobile clients may use an internal encrypted document store instead of exposing ordinary filesystem directories.

## 4. Identity and Authentication

### 4.1 OIDC Only

The platform must exclusively support OpenID Connect for interactive user authentication.

There must be:

- No built-in username/password authentication.
- No local registration or password recovery.
- No separate application-managed user passwords.
- No dependency on a specific identity provider.

The platform must support standards-compliant OIDC providers, including Authelia, Keycloak, and Authentik.

Users are provisioned automatically after successful authentication.

OIDC identities must be associated using the combination of issuer and subject (`iss` + `sub`), not email addresses alone.

### 4.2 Authorization

Authentication and authorization are separate.

The platform manages vault access and permissions internally, based on authenticated identities.

Users must not gain access to vaults simply by authenticating successfully.

### 4.3 Service and Agent Identities

Noninteractive clients, automation, and AI agents must have a supported authentication mechanism based on OIDC/OAuth standards.

Support scoped, revocable credentials and narrowly defined permissions.

Service identities must not bypass vault encryption or access controls.

## 5. Vault Management

A vault is an independent collection of Markdown documents, attachments, metadata, and encryption keys.

### Requirements

- Users may create unlimited vaults.
- Each vault has an owner.
- Vaults can be private or shared.
- Users may belong to multiple vaults.
- Vaults appear in the interfaces of authorized users.
- Vaults have independent encryption keys.
- Vaults have independent permissions and synchronization states.
- Each vault supports its own revision history.
- Vaults can be renamed, archived, exported, and deleted.
- Vault contents remain portable and recoverable without proprietary services.

A user's private vaults must not be visible to other ordinary users.

Administrators may manage the service without automatically gaining access to encrypted user content.

No arbitrary commercial limits may be imposed on vaults, users, or documents.

## 6. Sharing and Collaboration

### 6.1 Vault Sharing

Vault owners can invite other users and grant access to vaults.

Initial roles:

| Permission | Owner | Editor | Viewer |
|---|---|---|---|
| Read documents | Yes | Yes | Yes |
| Create documents | Yes | Yes | No |
| Edit documents | Yes | Yes | No |
| Delete documents | Yes | Yes | No |
| Manage members | Yes | No | No |
| Change permissions | Yes | No | No |
| Delete vault | Yes | No | No |

Permissions must be enforced server-side as well as reflected in the client interface.

### 6.2 Document and Folder Sharing

Support sharing selected documents or folders without granting access to an entire private vault.

Recipients must not gain access to unrelated content, metadata, attachments, or encryption keys.

The implementing agent must design an encryption and authorization model that supports this without compromising vault isolation.

If this significantly complicates the initial architecture, it may be implemented after vault-level sharing, but the initial design must accommodate it.

### 6.3 Collaborative Editing

Multiple authorized users must be able to edit the same document simultaneously.

Requirements:

- Near-real-time propagation of changes.
- Conflict-free collaboration where technically feasible.
- Offline changes safely reconcile after reconnection.
- No silent loss of user edits.
- Visible collaborator presence when online.
- Version history and document recovery.
- Protection against stale clients overwriting newer content.

The agent must evaluate suitable collaboration mechanisms, including CRDT-based approaches.

## 7. End-to-End Encryption

**End-to-end encryption is mandatory from the first release. It is not a future enhancement.**

### 7.1 Encryption Requirements

- All document contents and attachments are encrypted client-side before synchronization.
- The synchronization server stores only encrypted content.
- The server cannot decrypt user documents.
- Each vault has an independent encryption key.
- Encrypted content uses authenticated encryption.
- Encryption must use established, peer-reviewed cryptographic primitives and maintained libraries.
- Do not invent custom cryptographic algorithms.
- All network communication must additionally use TLS.
- Revision history and stored snapshots must also be encrypted.
- Filenames, paths, and sensitive document metadata must be protected from unnecessary disclosure.

The server may retain only the minimum metadata necessary to authenticate users, enforce permissions, route synchronization, and operate the service.

### 7.2 Key Management

The system must support secure distribution of vault encryption keys between authorized users and devices.

Requirements:

- User or device cryptographic identities.
- Secure key generation.
- Secure key storage.
- Encrypted key distribution.
- Device enrollment.
- Multiple devices per user.
- Key rotation.
- Recovery after device loss.
- Secure removal of devices.
- Revocation of vault access.

OIDC authentication must not grant the identity provider access to vault encryption keys.

### 7.3 Encrypted Sharing

Sharing a vault must securely distribute the necessary decryption material exclusively to authorized recipients.

The server must not be able to recover plaintext keys.

Removing a user must prevent access to future encrypted updates after key rotation.

The system must acknowledge that already downloaded data cannot be retroactively revoked.

### 7.4 Recovery

A user must not permanently lose access to all their notes merely because a single device is lost.

Provide an explicit recovery mechanism, such as recovery keys, encrypted key backups, or another appropriately secure approach.

The design must not introduce a server-side master decryption key.

### 7.5 Local Encryption

Local storage must be included in the security threat model.

The agent must distinguish between:

- Encrypted synchronization and server-side storage.
- Application-level encryption of local vault data.
- Filesystem or full-disk encryption provided by the operating system.

Desktop compatibility with ordinary Markdown files is important, but must not silently weaken the security guarantees.

If locally decrypted Markdown files are supported, this must be an explicit, user-controlled mode with clear security implications.

## 8. Markdown and Document Management

Markdown is the canonical portable representation of documents.

### Requirements

- Standard Markdown support.
- CommonMark or a clearly documented compatible dialect.
- Headings, lists, tables, code blocks, links, images, and attachments.
- Internal wiki-style links.
- Backlinks.
- Tags.
- Frontmatter or equivalent portable metadata.
- Folder-based organization.
- Document renaming and moving.
- Broken-link detection or safe link updates.
- Markdown import and export.
- Bulk vault import and export.
- No proprietary formatting required for ordinary documents.

The application must not require users to write code to configure navigation, menus, or document organization.

### Editor

The editor must provide:

- Rich-text editing.
- Markdown shortcuts.
- Source Markdown editing.
- Syntax-highlighted code blocks.
- Keyboard-first navigation.
- Keyboard shortcuts.
- Undo/redo.
- Autosave.
- Optional Vim keybindings.

The default editing experience must be immediately usable by nontechnical users.

## 9. Search and Knowledge Navigation

### Required

- Full-text search across locally accessible vaults.
- Search filtering by vault, folder, tag, and document.
- Backlinks.
- Internal document links.
- Recent documents.
- Favorites or pinned documents.
- Document tree or sidebar.
- Quick document switching.

Because the system uses E2EE, plaintext content indexing must happen on trusted clients rather than the untrusted server.

### Future Enhancements

- Interactive knowledge graph.
- Semantic/vector search.
- Saved searches.
- Advanced metadata queries.
- AI-assisted knowledge discovery.

## 10. Offline-First Synchronization

Native applications must work without a connection to the server.

### Requirements

- Documents are available locally after synchronization.
- Users can create, edit, rename, and organize notes offline.
- Changes are queued and synchronized when connectivity returns.
- Synchronization occurs automatically while clients are connected.
- Online edits propagate in near real time.
- Failed synchronization operations are retried safely.
- Synchronization is resumable after crashes or network interruptions.
- Duplicate operations must not cause duplicate or corrupted content.
- Conflicts must not result in silent data loss.
- The user can inspect synchronization status.

### Markdown Compatibility

The system must support portable Markdown representations while maintaining reliable synchronization.

The agent must explicitly address the relationship between:

- Local filesystem changes.
- In-application document edits.
- Collaborative editing state.
- Encrypted synchronization operations.
- Document snapshots.

External modifications made by tools such as Neovim or Obsidian must not silently corrupt synchronized documents.

## 11. AI Agents and External Integrations

The platform must be usable as a long-term knowledge and memory backend for AI agents.

### Headless API

Provide documented APIs to:

- List accessible vaults.
- List documents and folders.
- Read documents.
- Create documents.
- Update documents.
- Move and rename documents.
- Delete documents.
- Search accessible knowledge.
- Upload and download attachments.
- Retrieve document history.
- Subscribe to changes.

API operations must respect the same permissions as interactive clients.

### Encryption Boundary

Because the synchronization server cannot decrypt documents, plaintext APIs must operate through an authorized trusted client or agent gateway holding the appropriate decryption keys.

The agent gateway must:

- Act as an explicit cryptographic endpoint.
- Have access only to authorized vaults.
- Support restricted permissions.
- Never expose decryption keys through ordinary APIs.
- Support secure credential and key rotation.
- Provide auditable write operations.

### MCP

Provide an MCP-compatible integration layer allowing AI agents to access authorized knowledge.

MCP must not bypass the application's authentication, authorization, or encryption model.

Agents must be able to operate without access to every user's private notes.

## 12. User Experience

The application must be practical for ordinary family members, not just developers.

### Requirements

- Clean, responsive interface.
- Usable immediately after first login.
- Familiar document navigation.
- Simple vault creation.
- Simple sharing workflow.
- No mandatory configuration scripting.
- No requirement to understand Markdown to write a note.
- Clear permission indicators.
- Straightforward switching between private and shared vaults.
- Useful keyboard shortcuts for advanced users.
- Minimal setup and maintenance.

Advanced customization may exist, but essential functionality must never depend on users programming their own interface.

## 13. Deployment and Operations

### Requirements

- Fully self-hostable.
- Headless server.
- Containerized deployment.
- Docker Compose support.
- Kubernetes-compatible deployment.
- Configurable persistent storage.
- External OIDC configuration.
- Reverse-proxy compatibility.
- Health and readiness endpoints.
- Structured logs.
- Metrics and observability.
- Documented backup and restore procedures.
- Safe application upgrades.
- No telemetry by default.
- No reliance on proprietary hosted infrastructure.

The deployment should be simple enough for a single technically experienced person to maintain.

Avoid unnecessary microservices.

## 14. Licensing and Openness

The complete platform must be released under an OSI-approved open-source license.

Requirements:

- No mandatory commercial subscriptions.
- No per-user licensing fees.
- No paid feature gates for essential functionality.
- No proprietary synchronization protocol required for interoperability.
- No vendor-controlled account service.
- No externally hosted dependency required for normal operation.
- Documented data formats and APIs.

Third-party dependencies must be compatible with the project's selected license.

The implementing agent may recommend a suitable license and document the tradeoffs.

## 15. Security Requirements

Security must be treated as an architectural requirement, not an optional hardening phase.

### Mandatory

- OIDC-only authentication.
- Correct token validation and issuer/audience verification.
- Authorization checks on every relevant server operation.
- Client-side E2EE.
- Secure key handling.
- Secure device enrollment and removal.
- Encrypted synchronization.
- Protection against replay and rollback attacks.
- Safe handling of offline clients.
- Rate limiting and abuse protection.
- Input validation.
- Secure attachment handling.
- Protection against cross-site scripting and unauthorized document access.
- Dependency and supply-chain security checks.
- No sensitive information in logs or telemetry.

The design must include a documented threat model.

Threats should include compromised servers, stolen credentials, malicious collaborators, compromised devices, untrusted sync operations, and loss of encryption keys.

## 16. Nonfunctional Requirements

| Area | Requirement |
|---|---|
| Performance | Fast navigation and editing for ordinary-sized vaults |
| Scalability | Multiple users and vaults without architectural redesign |
| Reliability | No silent data loss |
| Security | End-to-end encrypted by default |
| Availability | Native clients remain usable offline |
| Portability | Human-readable Markdown import/export |
| Compatibility | Linux, macOS, Windows, iOS, Android |
| Maintainability | Minimal infrastructure and clear component boundaries |
| Extensibility | Documented APIs and integration mechanisms |
| Privacy | No telemetry or third-party tracking by default |
| Operations | Predictable backup, restore, and upgrades |

## 17. MVP Scope

The first usable release must include:

1. Rust-based headless synchronization server.
2. Web frontend with a complete, ready-to-use document interface.
3. OIDC authentication.
4. Multiple user accounts derived from OIDC identities.
5. Private and shared vaults.
6. Vault-level permissions and sharing.
7. End-to-end encryption, including secure key distribution and recovery.
8. Markdown creation and editing.
9. Live encrypted synchronization.
10. Offline-capable native desktop client, Linux first.
11. Concurrent document editing and conflict handling.
12. Local search and document navigation.
13. Import/export of Markdown vaults.
14. Documented API for integrations.
15. Secure backup and restore procedures.

The initial architecture must explicitly accommodate mobile clients, fine-grained sharing, and agent-based access even if the complete implementations arrive after the first desktop release.

## 18. Post-MVP Features

- Interactive knowledge graph.
- Advanced semantic search.
- Additional MCP tooling.
- Optional Vim mode, if not included earlier.
- Enhanced collaborative presence.
- Advanced document history and comparison.
- Optional plugins and extensions.

## 19. Explicit Non-Goals

The platform is not intended to become a complete productivity suite.

Initially exclude:

- Spreadsheet databases.
- Kanban boards.
- Project management.
- Calendar and scheduling.
- Built-in chat or messaging.
- Built-in LLM inference.
- AI model hosting.
- Workflow automation builders.
- Plugin marketplace.
- Public website publishing.
- Enterprise billing.
- Proprietary cloud services.

The focus is **knowledge management, privacy, synchronization, and collaboration**.

## 20. Engineering Expectations

The implementing agent is expected to make independent technical decisions within the constraints of this specification.

Before implementation, the agent must:

1. Evaluate architectural alternatives.
2. Propose a coherent system design.
3. Define the encryption and key-management model.
4. Define the offline and live synchronization model.
5. Explain how Markdown portability is maintained.
6. Identify security and data-loss risks.
7. Define milestones and acceptance tests.
8. Document significant technology decisions.

Prefer simple, maintainable solutions over unnecessary abstractions.

Use established libraries and protocols whenever possible.

Do not weaken the encryption, authorization, data-ownership, or multiuser requirements to simplify implementation.

Avoid unnecessary clarification requests when reasonable engineering decisions can be made independently. Explicitly document assumptions instead.

## 21. Release Acceptance Criteria

A first release is acceptable only when:

- Two independently authenticated users can create private vaults.
- Neither user can access the other's private content.
- One user can share a vault with the other.
- Both can collaboratively edit shared notes.
- Changes synchronize across connected clients.
- Clients can edit documents offline and synchronize later.
- No edits are silently lost during concurrent editing or reconnection.
- The server cannot decrypt note contents.
- Document bodies, attachments, and sensitive filenames are encrypted before upload.
- A revoked collaborator cannot access future content after key rotation.
- Device enrollment and recovery work without server-held plaintext vault keys.
- The web frontend can edit encrypted vaults.
- An authorized desktop client can read and edit Markdown locally.
- An authorized agent client can read and update permitted documents without exposing plaintext to the sync server.
- Vaults can be exported in portable Markdown format.
- Encrypted backups can be restored successfully.
- The application runs without proprietary cloud services or paid feature licenses.
- A new family member can start using the application without writing code or configuring custom scripts.

---

## Product Statement

**A self-hosted, open-source, local-first Markdown knowledge platform written primarily in Rust, combining Obsidian-style personal knowledge management with Notion-style collaboration, end-to-end encrypted synchronization, OIDC authentication, and secure access for humans and AI agents.**

**Architectural principle:** The server coordinates. The clients own the data and encryption keys.

**Development principle:** Build the smallest coherent system that fully satisfies the core security, usability, and collaboration requirements before expanding the feature set.
