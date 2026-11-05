<?php
/**
 * Ferro WP Adminer bootstrap: per-site auto-login.
 *
 * Served at https://<site>.localhost:<port>/__adminer/ — the request host
 * names the site, so there is no picker and no cookie. Credentials always
 * come from the ferro daemon over its Unix socket — never from the URL.
 *
 *   /__adminer/              -> straight into Adminer, logged in
 *   /__adminer/?ferro=other  -> stock Adminer login form (other databases)
 */

$bridge = __DIR__ . '/ferro-bridge.php';
if (!file_exists($bridge)) {
    http_response_code(500);
    exit('ferro: ferro-bridge.php is missing — re-run `ferro update adminer`');
}
require $bridge;

// --- stock login escape hatch ------------------------------------------
if (isset($_GET['ferro']) && $_GET['ferro'] === 'other') {
    header('Location: ' . ferro_clean_url() . '?username=');
    exit;
}

// --- host-named site must have a database ------------------------------
$site = ferro_adminer_site();
$stock_login = isset($_GET['username']) || isset($_GET['server']) || !empty($_POST['auth']);
if ($site && !$stock_login) {
    $info = ferro_rpc('site.info', ['name' => $site]);
    if (!$info || empty($info['site']['db_name'])) {
        http_response_code(404);
        exit("ferro: site '$site' has no database (plain parked folder) — "
            . 'use ' . ferro_clean_url() . '?ferro=other to log in manually');
    }
}

// --- hand over to Adminer ----------------------------------------------
function adminer_object()
{
    require_once __DIR__ . '/ferro-plugin.php';
    return new Adminer\Plugins([new AdminerFerro()]);
}

require __DIR__ . '/adminer.php';
