// Quick-capture confirmation. The host injects window.__PHORMINX_TOAST__
// before load and destroys the window after a few seconds.
const toast = window.__PHORMINX_TOAST__ || {title: 'Captured', detail: '', error: false};
document.getElementById('toast-title').textContent = toast.title;
document.getElementById('toast-detail').textContent = toast.error ? toast.detail : `Added to ${toast.detail}`;
document.querySelector('.toast').classList.toggle('error', Boolean(toast.error));
