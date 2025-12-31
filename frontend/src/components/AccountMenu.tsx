import { useState, useCallback } from 'react';
import type { PasskeyInfo, InviteToken } from '../types';
import { fetchPasskeys, deletePasskey, createInviteToken, logout } from '../api';

interface AccountMenuProps {
  onLogout: () => void;
}

export function AccountMenu({ onLogout }: AccountMenuProps) {
  const [isOpen, setIsOpen] = useState(false);
  const [passkeys, setPasskeys] = useState<PasskeyInfo[]>([]);
  const [inviteToken, setInviteToken] = useState<InviteToken | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Load passkeys when menu opens
  const handleToggle = useCallback(async () => {
    if (!isOpen) {
      setIsLoading(true);
      setError(null);
      try {
        const data = await fetchPasskeys();
        setPasskeys(data);
      } catch (err) {
        setError('Failed to load passkeys');
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
      // Still clear local state
      onLogout();
    }
  }, [onLogout]);

  // Format date
  const formatDate = (timestamp: number) => {
    return new Date(timestamp * 1000).toLocaleDateString();
  };

  return (
    <div className="account-menu">
      <button className="account-btn" onClick={handleToggle}>
        ⚙️ Account
      </button>

      {isOpen && (
        <div className="account-dropdown">
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
