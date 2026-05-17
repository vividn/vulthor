// Vulthor web pane front-end.
//
// Extracted from inline <script> blocks so the page can ship under a strict
// CSP (`script-src 'self'`) — see src/web.rs:apply_security_headers.
//
// Behavior is a union of the two former shells: register the PWA service
// worker, subscribe to /events for refresh notifications, and re-render the
// shell on selection changes. The email body is loaded into a sandboxed
// <iframe srcdoc> so untrusted markup cannot reach the parent origin.

(function () {
    // Per-launch token gating every request except /healthz. The HTML shell
    // is reached via /?t=<token>, so location.search reliably carries it.
    // EventSource can't set custom headers, so we ride the query param for
    // both SSE and fetch — same wire form the server-side middleware
    // accepts (?t=<token> | X-Vulthor-Token).
    const TOKEN = new URLSearchParams(window.location.search).get('t') || '';
    function withToken(path) {
        if (!TOKEN) return path;
        const sep = path.includes('?') ? '&' : '?';
        return path + sep + 't=' + encodeURIComponent(TOKEN);
    }

    if ('serviceWorker' in navigator) {
        window.addEventListener('load', function () {
            navigator.serviceWorker.register(withToken('/sw.js')).catch(function (err) {
                console.log('SW registration failed:', err);
            });
        });
    }

    let currentEmailId = null;
    let isLoading = false;

    const eventSource = new EventSource(withToken('/events'));
    eventSource.addEventListener('email-changed', function (event) {
        if (event.data !== currentEmailId && !isLoading) {
            loadEmailContent();
        }
    });
    eventSource.onerror = function (event) {
        console.log('SSE connection error:', event);
    };

    async function loadEmailContent() {
        if (isLoading) return;
        isLoading = true;
        try {
            const response = await fetch(withToken('/api/current-email'));
            const emailData = await response.json();
            if (emailData.has_email) {
                updateEmailDisplay(emailData);
            } else {
                showWelcomeMessage();
            }
            currentEmailId = emailData.email_id;
        } catch (error) {
            console.error('Error loading email:', error);
        } finally {
            isLoading = false;
        }
    }

    function ensureEmailLayout() {
        if (!document.querySelector('.app-banner')) {
            const banner = document.createElement('div');
            banner.className = 'app-banner';
            const head = document.createElement('img');
            head.src = '/vulthor_head.png';
            head.alt = 'Vulthor Bird';
            head.className = 'logo-bird';
            const letters = document.createElement('img');
            letters.src = '/vulthor_letters.png';
            letters.alt = 'Vulthor';
            letters.className = 'logo-text';
            banner.appendChild(head);
            banner.appendChild(letters);
            document.body.insertBefore(banner, document.body.firstChild);
        }
        const container = document.querySelector('.container');
        if (!container.querySelector('.email-header')) {
            container.className = 'container email-view';
            container.innerHTML =
                '<header class="email-header">' +
                '  <h1 class="email-subject"></h1>' +
                '  <div class="email-meta">' +
                '    <div class="email-from"></div>' +
                '    <div class="email-to"></div>' +
                '    <div class="email-date"></div>' +
                '  </div>' +
                '</header>' +
                '<iframe class="email-content" sandbox srcdoc=""></iframe>' +
                '<footer class="app-footer">' +
                '  <p>Served by <strong>Vulthor</strong> - TUI Email Client</p>' +
                '</footer>';
        }
    }

    function updateEmailDisplay(emailData) {
        document.title = 'Vulthor - ' + emailData.subject;
        ensureEmailLayout();

        document.querySelector('.email-subject').textContent = emailData.subject;
        document.querySelector('.email-from').textContent = '';
        const fromLabel = document.createElement('strong');
        fromLabel.textContent = 'From: ';
        document.querySelector('.email-from').appendChild(fromLabel);
        document.querySelector('.email-from').appendChild(document.createTextNode(emailData.from));

        document.querySelector('.email-to').textContent = '';
        const toLabel = document.createElement('strong');
        toLabel.textContent = 'To: ';
        document.querySelector('.email-to').appendChild(toLabel);
        document.querySelector('.email-to').appendChild(document.createTextNode(emailData.to));

        document.querySelector('.email-date').textContent = '';
        const dateLabel = document.createElement('strong');
        dateLabel.textContent = 'Date: ';
        document.querySelector('.email-date').appendChild(dateLabel);
        document.querySelector('.email-date').appendChild(document.createTextNode(emailData.date));

        const contentEl = document.querySelector('.email-content');
        if (contentEl && contentEl.tagName === 'IFRAME') {
            contentEl.srcdoc = emailData.body_html;
        }

        renderAttachments(emailData.attachments);
        document.querySelector('.container').className = 'container email-view';
    }

    function renderAttachments(attachments) {
        const existing = document.querySelector('.attachments-section');
        if (existing) {
            existing.remove();
        }
        if (!attachments || attachments.length === 0) {
            return;
        }
        const section = document.createElement('div');
        section.className = 'attachments-section';
        const heading = document.createElement('h3');
        heading.textContent = 'Attachments';
        section.appendChild(heading);
        const list = document.createElement('ul');
        list.className = 'attachments-list';
        attachments.forEach(function (attachment) {
            const item = document.createElement('li');
            item.className = 'attachment-item';
            const icon = document.createElement('span');
            icon.className = 'attachment-icon';
            icon.textContent = '\u{1F4CE}';
            const name = document.createElement('span');
            name.className = 'attachment-name';
            name.textContent = attachment.filename;
            const type = document.createElement('span');
            type.className = 'attachment-type';
            type.textContent = '(' + attachment.content_type + ')';
            const size = document.createElement('span');
            size.className = 'attachment-size';
            size.textContent = attachment.size;
            item.appendChild(icon);
            item.appendChild(name);
            item.appendChild(type);
            item.appendChild(size);
            list.appendChild(item);
        });
        section.appendChild(list);
        const content = document.querySelector('.email-content');
        if (content && content.parentNode) {
            content.parentNode.insertBefore(section, content.nextSibling);
        }
    }

    function showWelcomeMessage() {
        document.title = 'Vulthor - Email Client';
        const banner = document.querySelector('.app-banner');
        if (banner) {
            banner.remove();
        }
        if (!document.querySelector('.welcome-header')) {
            const container = document.querySelector('.container');
            container.className = 'container welcome-view';
            container.innerHTML =
                '<header class="welcome-header">' +
                '  <img src="/vulthor_bird.png" alt="Vulthor Logo" class="welcome-logo">' +
                '  <h1>Vulthor</h1>' +
                '  <h2>TUI Email Client</h2>' +
                '</header>' +
                '<main class="welcome-content">' +
                '  <div class="welcome-message">' +
                '    <h3>Welcome to Vulthor</h3>' +
                '    <p>No email is currently selected in the terminal interface.</p>' +
                '    <p>To view an email here:</p>' +
                '    <ol>' +
                '      <li>Navigate to an email in the terminal</li>' +
                '      <li>Select it with <kbd>Enter</kbd></li>' +
                '      <li>The email will appear on this page</li>' +
                '    </ol>' +
                '  </div>' +
                '  <div class="keybindings">' +
                '    <h3>Key Bindings</h3>' +
                '    <div class="keybinding-grid">' +
                '      <div class="keybinding"><kbd>j</kbd> / <kbd>k</kbd><span>Navigate up/down</span></div>' +
                '      <div class="keybinding"><kbd>h</kbd> / <kbd>l</kbd><span>Switch views</span></div>' +
                '      <div class="keybinding"><kbd>Tab</kbd><span>Switch panes</span></div>' +
                '      <div class="keybinding"><kbd>Enter</kbd><span>Select item</span></div>' +
                '      <div class="keybinding"><kbd>Alt+a</kbd><span>View attachments</span></div>' +
                '      <div class="keybinding"><kbd>?</kbd><span>Show help</span></div>' +
                '      <div class="keybinding"><kbd>q</kbd><span>Quit</span></div>' +
                '    </div>' +
                '  </div>' +
                '</main>' +
                '<footer class="app-footer">' +
                '  <p>Served by <strong>Vulthor</strong> - TUI Email Client</p>' +
                '</footer>';
        } else {
            document.querySelector('.container').className = 'container welcome-view';
        }
    }

    window.addEventListener('load', loadEmailContent);
})();
