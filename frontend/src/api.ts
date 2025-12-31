import type { Event, EventTypeSummary, Rule, Stats, Classification, Filters, AuthStatus, PasskeyInfo, InviteToken, NotificationLogEntry, NotificationStatus, TestNotificationResult } from './types';

const API_BASE = '/api';

function buildEventsQuery(filters: Filters, limit: number, offset: number): string {
  const params = new URLSearchParams();

  if (filters.classifications.size > 0 && filters.classifications.size < 3) {
    params.set('classification', Array.from(filters.classifications).join(','));
  }

  if (filters.eventTypes.size > 0) {
    params.set('event_type', Array.from(filters.eventTypes).join(','));
  }

  if (filters.search.trim()) {
    params.set('search', filters.search.trim());
  }

  params.set('limit', String(limit));
  params.set('offset', String(offset));

  return params.toString();
}

export async function fetchEvents(
  filters: Filters,
  limit: number = 200,
  offset: number = 0
): Promise<Event[]> {
  const query = buildEventsQuery(filters, limit, offset);
  const res = await fetch(`${API_BASE}/events?${query}`);
  if (!res.ok) throw new Error(`Failed to fetch events: ${res.status}`);
  return res.json();
}

export async function fetchEventPayload(eventId: string): Promise<unknown> {
  const res = await fetch(`${API_BASE}/events/${encodeURIComponent(eventId)}/payload`);
  if (!res.ok) throw new Error(`Failed to fetch payload: ${res.status}`);
  const data = await res.json();
  return data.payload;
}

export async function fetchEventTypes(): Promise<EventTypeSummary[]> {
  const res = await fetch(`${API_BASE}/events/types`);
  if (!res.ok) throw new Error(`Failed to fetch event types: ${res.status}`);
  return res.json();
}

export async function fetchRules(): Promise<Rule[]> {
  const res = await fetch(`${API_BASE}/rules`);
  if (!res.ok) throw new Error(`Failed to fetch rules: ${res.status}`);
  return res.json();
}

export async function setRule(eventType: string, classification: Classification): Promise<Rule> {
  const res = await fetch(`${API_BASE}/rules`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ event_type: eventType, classification }),
  });
  if (!res.ok) throw new Error(`Failed to set rule: ${res.status}`);
  return res.json();
}

export async function deleteRule(eventType: string): Promise<void> {
  const res = await fetch(`${API_BASE}/rules/${encodeURIComponent(eventType)}`, {
    method: 'DELETE',
  });
  if (!res.ok && res.status !== 404) {
    throw new Error(`Failed to delete rule: ${res.status}`);
  }
}

export async function fetchStats(): Promise<Stats> {
  const res = await fetch(`${API_BASE}/stats`);
  if (!res.ok) throw new Error(`Failed to fetch stats: ${res.status}`);
  return res.json();
}

export async function fetchEventCount(filters: Filters): Promise<number> {
  const params = new URLSearchParams();

  if (filters.classifications.size > 0 && filters.classifications.size < 3) {
    params.set('classification', Array.from(filters.classifications).join(','));
  }

  if (filters.eventTypes.size > 0) {
    params.set('event_type', Array.from(filters.eventTypes).join(','));
  }

  if (filters.search.trim()) {
    params.set('search', filters.search.trim());
  }

  const res = await fetch(`${API_BASE}/events/count?${params.toString()}`);
  if (!res.ok) throw new Error(`Failed to fetch event count: ${res.status}`);
  const data = await res.json();
  return data.count;
}

// ============================================================================
// Auth API
// ============================================================================

export async function fetchAuthStatus(): Promise<AuthStatus> {
  const res = await fetch(`${API_BASE}/auth/status`);
  if (!res.ok) throw new Error(`Failed to fetch auth status: ${res.status}`);
  return res.json();
}

export async function startRegistration(token?: string, name?: string): Promise<{
  challenge: { publicKey: PublicKeyCredentialCreationOptions };
  challenge_id: string;
}> {
  const res = await fetch(`${API_BASE}/auth/register/start`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ token, name }),
  });
  if (!res.ok) {
    const error = await res.json();
    throw new Error(error.error || `Registration failed: ${res.status}`);
  }
  return res.json();
}

export async function finishRegistration(
  challengeId: string,
  credential: PublicKeyCredential,
  name?: string
): Promise<{ success: boolean }> {
  const res = await fetch(`${API_BASE}/auth/register/finish`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      challenge_id: challengeId,
      credential: publicKeyCredentialToJSON(credential),
      name,
    }),
  });
  if (!res.ok) {
    const error = await res.json();
    throw new Error(error.error || `Registration failed: ${res.status}`);
  }
  return res.json();
}

export async function startLogin(): Promise<{
  challenge: { publicKey: PublicKeyCredentialRequestOptions };
  challenge_id: string;
}> {
  const res = await fetch(`${API_BASE}/auth/login/start`, {
    method: 'POST',
  });
  if (!res.ok) {
    const error = await res.json();
    throw new Error(error.error || `Login failed: ${res.status}`);
  }
  return res.json();
}

export async function finishLogin(
  challengeId: string,
  credential: PublicKeyCredential
): Promise<{ success: boolean }> {
  const res = await fetch(`${API_BASE}/auth/login/finish`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      challenge_id: challengeId,
      credential: publicKeyCredentialToJSON(credential),
    }),
  });
  if (!res.ok) {
    const error = await res.json();
    throw new Error(error.error || `Login failed: ${res.status}`);
  }
  return res.json();
}

export async function logout(): Promise<void> {
  const res = await fetch(`${API_BASE}/auth/logout`, {
    method: 'POST',
  });
  if (!res.ok) throw new Error(`Logout failed: ${res.status}`);
}

export async function fetchPasskeys(): Promise<PasskeyInfo[]> {
  const res = await fetch(`${API_BASE}/auth/passkeys`);
  if (!res.ok) throw new Error(`Failed to fetch passkeys: ${res.status}`);
  return res.json();
}

export async function deletePasskey(id: string): Promise<void> {
  const res = await fetch(`${API_BASE}/auth/passkeys/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  });
  if (!res.ok && res.status !== 404) {
    throw new Error(`Failed to delete passkey: ${res.status}`);
  }
}

export async function createInviteToken(): Promise<InviteToken> {
  const res = await fetch(`${API_BASE}/auth/invite`, {
    method: 'POST',
  });
  if (!res.ok) throw new Error(`Failed to create invite: ${res.status}`);
  return res.json();
}

// ============================================================================
// WebAuthn Helpers
// ============================================================================

function arrayBufferToBase64Url(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  let str = '';
  for (const byte of bytes) {
    str += String.fromCharCode(byte);
  }
  return btoa(str).replace(/\+/g, '-').replace(/\//g, '_').replace(/=/g, '');
}

function base64UrlToArrayBuffer(base64url: string): ArrayBuffer {
  const base64 = base64url.replace(/-/g, '+').replace(/_/g, '/');
  const paddedBase64 = base64 + '='.repeat((4 - (base64.length % 4)) % 4);
  const binary = atob(paddedBase64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes.buffer;
}

function publicKeyCredentialToJSON(credential: PublicKeyCredential): unknown {
  const response = credential.response;

  if ('attestationObject' in response) {
    // Registration response
    const attestationResponse = response as AuthenticatorAttestationResponse;
    return {
      id: credential.id,
      rawId: arrayBufferToBase64Url(credential.rawId),
      type: credential.type,
      response: {
        clientDataJSON: arrayBufferToBase64Url(attestationResponse.clientDataJSON),
        attestationObject: arrayBufferToBase64Url(attestationResponse.attestationObject),
      },
    };
  } else {
    // Authentication response
    const assertionResponse = response as AuthenticatorAssertionResponse;
    return {
      id: credential.id,
      rawId: arrayBufferToBase64Url(credential.rawId),
      type: credential.type,
      response: {
        clientDataJSON: arrayBufferToBase64Url(assertionResponse.clientDataJSON),
        authenticatorData: arrayBufferToBase64Url(assertionResponse.authenticatorData),
        signature: arrayBufferToBase64Url(assertionResponse.signature),
        userHandle: assertionResponse.userHandle
          ? arrayBufferToBase64Url(assertionResponse.userHandle)
          : null,
      },
    };
  }
}

export function prepareCreationOptions(
  options: PublicKeyCredentialCreationOptions
): PublicKeyCredentialCreationOptions {
  return {
    ...options,
    challenge: base64UrlToArrayBuffer(options.challenge as unknown as string),
    user: {
      ...options.user,
      id: base64UrlToArrayBuffer(options.user.id as unknown as string),
    },
    excludeCredentials: options.excludeCredentials?.map((cred) => ({
      ...cred,
      id: base64UrlToArrayBuffer(cred.id as unknown as string),
    })),
  };
}

export function prepareRequestOptions(
  options: PublicKeyCredentialRequestOptions
): PublicKeyCredentialRequestOptions {
  return {
    ...options,
    challenge: base64UrlToArrayBuffer(options.challenge as unknown as string),
    allowCredentials: options.allowCredentials?.map((cred) => ({
      ...cred,
      id: base64UrlToArrayBuffer(cred.id as unknown as string),
    })),
  };
}

// ============================================================================
// Notifications API
// ============================================================================

export async function fetchNotificationStatus(): Promise<NotificationStatus> {
  const res = await fetch(`${API_BASE}/notifications/status`);
  if (!res.ok) throw new Error(`Failed to fetch notification status: ${res.status}`);
  return res.json();
}

export async function fetchNotificationHistory(limit: number = 50): Promise<NotificationLogEntry[]> {
  const res = await fetch(`${API_BASE}/notifications/history?limit=${limit}`);
  if (!res.ok) throw new Error(`Failed to fetch notification history: ${res.status}`);
  return res.json();
}

export async function sendTestNotification(): Promise<TestNotificationResult> {
  const res = await fetch(`${API_BASE}/notifications/test`, {
    method: 'POST',
  });
  if (!res.ok) throw new Error(`Failed to send test notification: ${res.status}`);
  return res.json();
}
