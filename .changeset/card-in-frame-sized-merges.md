---
default: patch
---

# Publish the agent card in frame-sized merges

On a gossip with a password, the card of a member was larger than one frame, so it was never published, and every directed message and task to that member failed with "cards still propagating".

The card now goes out in several merges, each within one frame, cut by the measured size of the frame, and the capabilities come first, because they hold the seal key. A merge that fails is tried again, with a longer wait after each failure. The reason is in `/peers/<nick>/card_error`, and a sender that cannot seal to a member names it.

Between two of the merges, `/peers/<nick>/card` holds only the first keys of the card. The card route of the local A2A binding withholds a card that lacks a field of the agent card schema, and a reader that needs the whole card checks the same.
