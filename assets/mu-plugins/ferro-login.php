<?php
/**
 * Plugin Name: Ferro WP — one-click admin login
 * Description: Accepts a one-time, short-lived token from the ferro daemon
 *   (via ?ferro-login=) and logs you into wp-admin. Loopback + local
 *   environment only; tokens are single-use and expire in 60 seconds.
 *
 * Installed by `ferro new`. Safe to delete if you don't want one-click login.
 */

add_action('init', function () {
    if (empty($_GET['ferro-login'])) {
        return;
    }
    // Local environments only.
    if (!defined('WP_ENVIRONMENT_TYPE') || WP_ENVIRONMENT_TYPE !== 'local') {
        return;
    }
    // Loopback only.
    $addr = $_SERVER['REMOTE_ADDR'] ?? '';
    if (!in_array($addr, ['127.0.0.1', '::1'], true)) {
        return;
    }
    $token = preg_replace('/[^a-f0-9]/', '', (string) $_GET['ferro-login']);
    if (strlen($token) !== 64) {
        return;
    }
    $home = getenv('HOME');
    if (!$home) {
        return;
    }
    $sock_path = $home . '/Library/Application Support/ferro/ferrod.sock';
    if (!file_exists($sock_path)) {
        return;
    }
    $fp = @fsockopen('unix://' . $sock_path, 0, $errno, $errstr, 1.0);
    if (!$fp) {
        return;
    }
    fwrite($fp, json_encode([
        'id' => 1,
        'method' => 'login.consume',
        'params' => ['token' => $token],
    ]) . "\n");
    $line = fgets($fp);
    fclose($fp);
    $reply = json_decode($line ?: '', true);
    $info = $reply['result'] ?? null;
    if (!$info) {
        wp_die('ferro: login token is invalid or expired (tokens last 60 seconds and are single-use)');
    }
    // The token is bound to a site; only honor it on that site's host.
    $host = strtok($_SERVER['HTTP_HOST'] ?? '', ':');
    if (!empty($info['site']) && strpos($host, $info['site'] . '.') !== 0) {
        wp_die('ferro: login token was issued for a different site');
    }

    $user = null;
    if (!empty($info['user'])) {
        $user = get_user_by('login', $info['user']);
    }
    if (!$user) {
        $admins = get_users(['role' => 'administrator', 'number' => 1, 'orderby' => 'ID']);
        $user = $admins[0] ?? null;
    }
    if (!$user) {
        wp_die('ferro: no administrator account found on this site');
    }

    wp_set_current_user($user->ID, $user->user_login);
    wp_set_auth_cookie($user->ID, true);
    do_action('wp_login', $user->user_login, $user);
    wp_safe_redirect(admin_url());
    exit;
}, 1);
