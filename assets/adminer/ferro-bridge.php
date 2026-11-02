<?php
/**
 * ferro <-> Adminer bridge: talk to the ferro daemon over its Unix socket.
 */

function ferro_rpc(string $method, array $params = []): ?array
{
    $home = getenv('HOME');
    if (!$home) {
        return null;
    }
    $sock = $home . '/Library/Application Support/ferro/ferrod.sock';
    if (!file_exists($sock)) {
        return null;
    }
    $fp = @fsockopen('unix://' . $sock, 0, $errno, $errstr, 2.0);
    if (!$fp) {
        return null;
    }
    fwrite($fp, json_encode(['id' => 1, 'method' => $method, 'params' => $params]) . "\n");
    $line = fgets($fp);
    fclose($fp);
    $response = json_decode($line ?: '', true);
    return $response['result'] ?? null;
}

/** Current URL without its query string. */
function ferro_clean_url(): string
{
    return strtok($_SERVER['REQUEST_URI'] ?? '/', '?') ?: '/';
}

/**
 * DB credentials for the picked site, fresh from the daemon.
 * Never read from the URL, never written to disk by this file.
 */
function ferro_adminer_creds(): ?array
{
    $site = $_COOKIE['ferro_site'] ?? null;
    if (!$site || !preg_match('/^[a-z0-9-]{1,63}$/', $site)) {
        return null;
    }
    $info = ferro_rpc('site.info', ['name' => $site]);
    if (!$info || empty($info['site']['db_user'])) {
        return null;
    }
    $port = $info['mysql_port'] ?? 3307;
    return [
        'server' => '127.0.0.1:' . $port,
        'user' => $info['site']['db_user'],
        'pass' => $info['site']['db_pass'],
        'db' => $info['site']['db_name'],
        'site' => $site,
    ];
}
