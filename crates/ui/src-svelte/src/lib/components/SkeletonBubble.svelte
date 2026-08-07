<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<div class="skeleton" aria-hidden="true"></div>

<style>
  .skeleton {
    background: var(--bg-elevated);
    border-radius: 12px;
    margin: var(--s-1) 0;
    height: 56px;
    max-width: 60ch;
    animation: pulse 1.6s ease-in-out infinite;
    opacity: 0.6;
  }
  @keyframes pulse {
    0%, 100% { opacity: 0.4; }
    50% { opacity: 0.7; }
  }
  @media (prefers-reduced-motion: reduce) {
    .skeleton { animation: none; opacity: 0.5; }
  }
</style>
