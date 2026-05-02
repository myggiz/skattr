<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<div class="separator" role="separator" aria-label="Unread messages below">
  <span class="line"></span>
  <span class="label">Unread</span>
  <span class="line"></span>
</div>

<style>
  .separator {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    margin: var(--s-3) 0;
    color: var(--accent);
    font: var(--t-ui);
  }
  .line {
    flex: 1;
    height: 1px;
    background: var(--accent);
    opacity: 0.4;
  }
  .label {
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-size: 0.75rem;
  }
</style>
