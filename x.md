  - Participants are only visible in the collaboration dialog. While editing, the tab badge and status pill show a count, but there is no persistent avatar stack or obvious “who is here?” control near the document.
  - Solution: This seems to call for a top to bottom collab UX stack, where clients can define their identities, profile pictures, and possibly even preferred colors durably. It also implies UI elements indicating presence. These are probably desirable.

  - Permissions are currently implicit. Every generated invite is an Editor invite, and the UI does not let the owner choose Viewer vs. Editor. The code already has capability roles; the Share UI simply hardcodes Editor today.
  - Solution: All collaborators are editors is an intentional simplification for the debate workflow. We do not intend to change it, and we should leave it unchanged.

  - There is no in-document social layer. Users can see colored remote carets and join/leave notifications, but there are no comments, mentions, chat, or activity history.
    That may be intentional for now, but it limits collaboration to simultaneous editing.
  - Query: How much of a lift would it be to add comments that feel nice and production-ready? The other stuff is irrelevant. Secondarily: isn't activity history represented by content provenance of the peers in the edit history of a Loro doc? Or is that list flat and not contain identity?

  - The invite is a raw text ticket. The flow is copy/paste-oriented: copy a long encoded string, send it elsewhere, and paste it into Flowstate. There is no friendlier link, QR code, contact picker, or clear expiry/permission summary.
  - Solution: Agreed, but I do not know what the full frontier of discovery is and ought to be. Obviously we should maintain some form of link discovery. Is it possible to allow clients to establish each other as 'trusted' to join each other's sessions at will? Is it possible to use the Dropbox SDK to identify when two clients are working on the same synced Dropbox document, exchange discovery automatically via Dropbox SDK, and propose initiating the session seamlessly with a single click? (Most debate squads use Dropbox for the squad). What about Bluetooth discovery of nearby advertising identities for when you need to quickly link up with a partner? Is there a friendlier way to present the copy-paste link disocvery (or really just: a friendlier way to permit the total fallback discovery in general?)

  - Session management is dialog-heavy. Starting, inviting, checking participants, and leaving all happen through the Share/Collaborate dialog or a small status pill. There is not yet a dedicated collaboration sidebar or lightweight popover that stays useful while editing.
  - Agreed, but let's not edit this until we have a locked down vision for the rest of the ux, since this is heavily ux dependent.
