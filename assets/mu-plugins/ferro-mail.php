<?php
/**
 * Ferro WP: route wp_mail() to the local Mailpit catcher.
 *
 * Mailpit runs on 127.0.0.1:1025 (UI at https://mail.localhost:8443).
 * Only active when FERRO is defined (set by ferro new / ferro pull), so
 * a site copied to production keeps its own mail setup.
 */

if (!defined('FERRO') || !FERRO) {
    return;
}

add_action('phpmailer_init', function ($phpmailer) {
    $phpmailer->isSMTP();
    $phpmailer->Host = '127.0.0.1';
    $phpmailer->Port = 1025;
    $phpmailer->SMTPAuth = false;
    $phpmailer->SMTPAutoTLS = false;
}, 10, 1);
