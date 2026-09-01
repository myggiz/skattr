<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<div class="separator" role="separator" aria-label="Unread messages below">
  <span class="line"></span>
  <span class="node"></span>
  <span class="label">Unread</span>
  <span class="node"></span>
  <span class="line"></span>
</div>

<style>
  /* The same hairline-and-node motif as the circuit trace in the header: one
     idea reused where it is true, rather than a second decorative device. */
  .separator {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    margin: var(--s-3) 0;
    color: var(--accent);
  }
  .line {
    flex: 1;
    height: 1px;
    background: var(--hairline);
  }
  .node {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: currentColor;
    flex: none;
  }
  .label {
    font: var(--t-label);
    text-transform: uppercase;
    letter-spacing: 0.12em;
  }
</style>
