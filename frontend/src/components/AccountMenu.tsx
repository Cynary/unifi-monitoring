import { useState, useCallback } from 'react';
import type { PasskeyInfo, InviteToken, NotificationLogEntry, NotificationStatus } from '../types';
import { fetchPasskeys, deletePasskey, createInviteToken, logout, fetchNotificationStatus, fetchNotificationHistory, sendTestNotification } from '../api';

interface AccountMenuProps {
  onLogout: () => void;
  theme: 'light' | 'dark';
  onToggleTheme: () => void;
}

export function AccountMenu({ onLogout, theme, onToggleTheme }: AccountMenuProps) {
  const [isOpen, setIsOpen] = useState(false);
  const [passkeys, setPasskeys] = useState<PasskeyInfo[]>([]);
  const [inviteToken, setInviteToken] = useState<InviteToken | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Notification state
  const [notificationStatus, setNotificationStatus] = useState<NotificationStatus | null>(null);
  const [notificationHistory, setNotificationHistory] = useState<NotificationLogEntry[]>([]);
  const [isSendingTest, setIsSendingTest] = useState(false);
  const [testResult, setTestResult] = useState<{ success: boolean; error?: string } | null>(null);

  // Load data when menu opens
  const handleToggle = useCallback(async () => {
    if (!isOpen) {
      setIsLoading(true);
      setError(null);
      setTestResult(null);
      try {
        const [passkeysData, notifStatus, notifHistory] = await Promise.all([
          fetchPasskeys(),
          fetchNotificationStatus(),
          fetchNotificationHistory(10),
        ]);
        setPasskeys(passkeysData);
        setNotificationStatus(notifStatus);
        setNotificationHistory(notifHistory);
      } catch (err) {
        setError('Failed to load account data');
      } finally {
        setIsLoading(false);
      }
    }
    setIsOpen(!isOpen);
  }, [isOpen]);

  // Generate invite token
  const handleCreateInvite = useCallback(async () => {
    setError(null);
    try {
      const token = await createInviteToken();
      setInviteToken(token);
    } catch (err) {
      setError('Failed to create invite');
    }
  }, []);

  // Delete a passkey
  const handleDeletePasskey = useCallback(async (id: string) => {
    if (passkeys.length <= 1) {
      setError('Cannot delete the last passkey');
      return;
    }
    if (!confirm('Delete this passkey?')) return;

    try {
      await deletePasskey(id);
      setPasskeys((prev) => prev.filter((p) => p.id !== id));
    } catch (err) {
      setError('Failed to delete passkey');
    }
  }, [passkeys.length]);

  // Handle logout
  const handleLogout = useCallback(async () => {
    try {
      await logout();
      onLogout();
    } catch (err) {
      console.error('Logout failed:', err);
      onLogout();
    }
  }, [onLogout]);

  // Send test notification
  const handleTestNotification = useCallback(async () => {
    setIsSendingTest(true);
    setTestResult(null);
    setError(null);
    try {
      const result = await sendTestNotification();
      setTestResult({ success: result.success, error: result.error || undefined });
      // Refresh history after test
      const history = await fetchNotificationHistory(10);
      setNotificationHistory(history);
    } catch (err) {
      setTestResult({ success: false, error: 'Failed to send test notification' });
    } finally {
      setIsSendingTest(false);
    }
  }, []);

  // Format date
  const formatDate = (timestamp: number) => {
    return new Date(timestamp * 1000).toLocaleDateString();
  };

  // Format datetime for notifications
  const formatDateTime = (timestamp: number) => {
    return new Date(timestamp * 1000).toLocaleString();
  };

  return (
    <div className="account-menu">
      <button className="account-btn" onClick={handleToggle}>
        ⚙️ Account
      </button>

      {isOpen && (
        <div className="account-dropdown">
          {/* Telegram Notifications Section */}
          <div className="account-section">
            <h3>Telegram Notifications</h3>
            {isLoading ? (
              <p className="account-loading">Loading...</p>
            ) : notificationStatus?.configured ? (
              <>
                <div className="notification-test">
                  <button
                    className="test-notification-btn"
                    onClick={handleTestNotification}
                    disabled={isSendingTest}
                  >
                    {isSendingTest ? 'Sending...' : 'Send Test Notification'}
                  </button>
                  {testResult && (
                    <span className={`test-result ${testResult.success ? 'success' : 'error'}`}>
                      {testResult.success ? '✓ Sent!' : `✗ ${testResult.error}`}
                    </span>
                  )}
                </div>
                {notificationHistory.length > 0 && (
                  <div className="notification-history">
                    <h4>Recent Notifications</h4>
                    <ul className="notification-list">
                      {notificationHistory.map((entry) => (
                        <li key={entry.id} className={`notification-item ${entry.status}`}>
                          <span className="notification-status">
                            {entry.status === 'sent' ? '✓' : '✗'}
                          </span>
                          <span className="notification-summary">
                            {entry.event_summary || entry.event_type || 'Unknown'}
                          </span>
                          <span className="notification-time">
                            {formatDateTime(entry.created_at)}
                          </span>
                          {entry.error_message && (
                            <span className="notification-error" title={entry.error_message}>
                              Error
                            </span>
                          )}
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
              </>
            ) : (
              <p className="not-configured">
                Not configured. Set TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID environment variables.
              </p>
            )}
          </div>

          {/* Passkeys Section */}
          <div className="account-section">
            <h3>Passkeys</h3>
            {isLoading ? (
              <p className="account-loading">Loading...</p>
            ) : (
              <ul className="passkey-list">
                {passkeys.map((pk) => (
                  <li key={pk.id} className="passkey-item">
                    <span className="passkey-name">
                      {pk.name || 'Unnamed passkey'}
                    </span>
                    <span className="passkey-date">
                      Added {formatDate(pk.created_at)}
                    </span>
                    {passkeys.length > 1 && (
                      <button
                        className="passkey-delete"
                        onClick={() => handleDeletePasskey(pk.id)}
                        title="Delete passkey"
                      >
                        ×
                      </button>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </div>

          <div className="account-section">
            <h3>Add Passkey on Another Device</h3>
            {inviteToken ? (
              <div className="invite-token-box">
                <p className="invite-hint">
                  Share this code (expires in {Math.floor(inviteToken.expires_in_secs / 60)} min):
                </p>
                <code className="invite-token">{inviteToken.token}</code>
                <button
                  className="invite-copy-btn"
                  onClick={() => {
                    navigator.clipboard.writeText(inviteToken.token);
                  }}
                >
                  Copy
                </button>
              </div>
            ) : (
              <button className="invite-btn" onClick={handleCreateInvite}>
                Generate Invite Code
              </button>
            )}
          </div>

          <div className="account-section">
            <h3>Appearance</h3>
            <div className="theme-toggle">
              <span>Theme</span>
              <button className="theme-toggle-btn" onClick={onToggleTheme}>
                {theme === 'dark' ? '🌙 Dark' : '☀️ Light'}
              </button>
            </div>
          </div>

          {error && <p className="account-error">{error}</p>}

          <div className="account-section account-footer">
            <button className="logout-btn" onClick={handleLogout}>
              Logout
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
