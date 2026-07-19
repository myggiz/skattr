-- SPDX-License-Identifier: GPL-3.0-or-later
CREATE TABLE first_contact_acks (
    kp_ref        BLOB PRIMARY KEY NOT NULL,
    peer_x25519   BLOB NOT NULL,
    peer_identity BLOB NOT NULL,
    created_at    INTEGER NOT NULL
);
