import { useState, useCallback } from 'react';
import type { AuthStatus } from '../types';
import {
  startRegistration,
  finishRegistration,
  startLogin,
  finishLogin,
  prepareCreationOptions,
  prepareRequestOptions,
} from '../api';

interface LoginProps {
  authStatus: AuthStatus;
  onAuthSuccess: () => void;
}

export function Login({ authStatus, onAuthSuccess }: LoginProps) {
  const [setupToken, setSetupToken] = useState('');
  const [inviteToken, setInviteToken] = useState('');
  const [passkeyName, setPasskeyName] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [showInviteForm, setShowInviteForm] = useState(false);

  // Handle passkey registration (first setup or with invite token)
  const handleRegister = useCallback(async () => {
    setError(null);
    setIsLoading(true);

    try {
      // Start registration
      const { challenge, challenge_id } = await startRegistration(
        authStatus.needs_setup ? setupToken : undefined,
        passkeyName || undefined
      );

      // Prepare options for browser API (webauthn-rs returns nested publicKey)
      const options = prepareCreationOptions(challenge.publicKey);

      // Call browser WebAuthn API
      const credential = await navigator.credentials.create({
        publicKey: options,
      });

      if (!credential || !(credential instanceof PublicKeyCredential)) {
        throw new Error('Failed to create credential');
      }

      // Finish registration
      await finishRegistration(challenge_id, credential, passkeyName || undefined);

      // Success!
      onAuthSuccess();
    } catch (err) {
      console.error('Registration error:', err);
      setError(err instanceof Error ? err.message : 'Registration failed');
    } finally {
      setIsLoading(false);
    }
  }, [authStatus.needs_setup, setupToken, passkeyName, onAuthSuccess]);

  // Handle passkey login
  const handleLogin = useCallback(async () => {
    setError(null);
    setIsLoading(true);

    try {
      // Start login
      const { challenge, challenge_id } = await startLogin();

      // Prepare options for browser API (webauthn-rs returns nested publicKey)
      const options = prepareRequestOptions(challenge.publicKey);

      // Call browser WebAuthn API
      const credential = await navigator.credentials.get({
        publicKey: options,
      });

      if (!credential || !(credential instanceof PublicKeyCredential)) {
        throw new Error('Failed to get credential');
      }

      // Finish login
      await finishLogin(challenge_id, credential);

      // Success!
      onAuthSuccess();
    } catch (err) {
      console.error('Login error:', err);
      setError(err instanceof Error ? err.message : 'Login failed');
    } finally {
      setIsLoading(false);
    }
  }, [onAuthSuccess]);

  // Handle invite token registration
  const handleInviteRegister = useCallback(async () => {
    setError(null);
    setIsLoading(true);

    try {
      // Start registration with invite token
      const { challenge, challenge_id } = await startRegistration(
        inviteToken,
        passkeyName || undefined
      );

      // Prepare options for browser API
      const options = prepareCreationOptions(challenge.publicKey);

      // Call browser WebAuthn API
      const credential = await navigator.credentials.create({
        publicKey: options,
      });

      if (!credential || !(credential instanceof PublicKeyCredential)) {
        throw new Error('Failed to create credential');
      }

      // Finish registration
      await finishRegistration(challenge_id, credential, passkeyName || undefined);

      // Success!
      onAuthSuccess();
    } catch (err) {
      console.error('Registration error:', err);
      setError(err instanceof Error ? err.message : 'Registration failed');
    } finally {
      setIsLoading(false);
    }
  }, [inviteToken, passkeyName, onAuthSuccess]);

  // Handle Enter key in setup form
  const handleSetupKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && setupToken.trim() && !isLoading) {
      handleRegister();
    }
  }, [setupToken, isLoading, handleRegister]);

  // Handle Enter key in invite form
  const handleInviteKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && inviteToken.trim() && !isLoading) {
      handleInviteRegister();
    }
  }, [inviteToken, isLoading, handleInviteRegister]);

  // First-time setup: need to enter setup token
  if (authStatus.needs_setup) {
    return (
      <div className="login-container">
        <div className="login-box">
          <h1>UniFi Event Monitor</h1>
          <h2>Initial Setup</h2>
          <p className="login-hint">
            Enter the setup token from <code>data/setup-token.txt</code> to register your first passkey.
          </p>

          <div className="login-form">
            <input
              type="text"
              placeholder="Setup token"
              value={setupToken}
              onChange={(e) => setSetupToken(e.target.value)}
              onKeyDown={handleSetupKeyDown}
              disabled={isLoading}
              autoFocus
            />
            <input
              type="text"
              placeholder="Passkey name (optional, e.g., 'MacBook Pro')"
              value={passkeyName}
              onChange={(e) => setPasskeyName(e.target.value)}
              onKeyDown={handleSetupKeyDown}
              disabled={isLoading}
            />
            <button
              onClick={handleRegister}
              disabled={isLoading || !setupToken.trim()}
            >
              {isLoading ? 'Registering...' : 'Register Passkey'}
            </button>
          </div>

          {error && <p className="login-error">{error}</p>}
        </div>
      </div>
    );
  }

  // Normal login (with option to add new device via invite)
  if (showInviteForm) {
    return (
      <div className="login-container">
        <div className="login-box">
          <h1>UniFi Event Monitor</h1>
          <h2>Add New Device</h2>
          <p className="login-hint">
            Enter the invite code to register a passkey on this device.
          </p>

          <div className="login-form">
            <input
              type="text"
              placeholder="Invite code (e.g., alpha-bravo-charlie-delta)"
              value={inviteToken}
              onChange={(e) => setInviteToken(e.target.value)}
              onKeyDown={handleInviteKeyDown}
              disabled={isLoading}
              autoFocus
            />
            <input
              type="text"
              placeholder="Device name (optional, e.g., 'iPhone')"
              value={passkeyName}
              onChange={(e) => setPasskeyName(e.target.value)}
              onKeyDown={handleInviteKeyDown}
              disabled={isLoading}
            />
            <button
              onClick={handleInviteRegister}
              disabled={isLoading || !inviteToken.trim()}
            >
              {isLoading ? 'Registering...' : 'Register Passkey'}
            </button>
            <button
              type="button"
              className="login-secondary-btn"
              onClick={() => {
                setShowInviteForm(false);
                setError(null);
              }}
              disabled={isLoading}
            >
              Back to Login
            </button>
          </div>

          {error && <p className="login-error">{error}</p>}
        </div>
      </div>
    );
  }

  return (
    <div className="login-container">
      <div className="login-box">
        <h1>UniFi Event Monitor</h1>
        <h2>Login</h2>
        <p className="login-hint">
          Use your passkey to authenticate.
        </p>

        <div className="login-form">
          <button
            onClick={handleLogin}
            disabled={isLoading}
            className="login-passkey-btn"
          >
            {isLoading ? 'Authenticating...' : 'Login with Passkey'}
          </button>
          <button
            type="button"
            className="login-secondary-btn"
            onClick={() => {
              setShowInviteForm(true);
              setError(null);
            }}
            disabled={isLoading}
          >
            Add this device with invite code
          </button>
        </div>

        {error && <p className="login-error">{error}</p>}
      </div>
    </div>
  );
}
