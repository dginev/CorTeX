$(document).ready(function () {
  var queued_count = parseInt($("td.queued").text());
  if (queued_count > 0) {
    // conversion in progress, create temporary rerun report
    var no_problem = parseInt($("td.no-problem").text());
    var warning = parseInt($("td.warning").text());
    var error = parseInt($("td.error").text());
    var fatal = parseInt($("td.fatal").text());
    var total = no_problem + warning + error + fatal;
    var report_div = $("div#report-div");
    report_div.prepend("<br><h2>Full Corpus</h2>");

    var td_a_no_problem = $("tr.corpus-report-no-problems").find('>:first-child');
    var tr_no_problem = $('<tr class="success corpus-report-no-problems" />');
    tr_no_problem.append(td_a_no_problem.clone());
    tr_no_problem.append('<td class="right">' + ((100.0 * no_problem) / total).toFixed(2) + '%</td>');
    tr_no_problem.append('<td class="right no-problem">' + no_problem + '</td>');

    var td_a_warning = $("tr.corpus-report-warnings").find('>:first-child');
    var tr_warning = $('<tr class="warning corpus-report-warnings" />');
    tr_warning.append(td_a_warning.clone());
    tr_warning.append('<td class="right">' + ((100.0 * warning) / total).toFixed(2) + '%</td>');
    tr_warning.append('<td class="right no-problem">' + warning + '</td>');

    var td_a_error = $("tr.corpus-report-errors").find('>:first-child');
    var tr_error = $('<tr class="error corpus-report-errors" />');
    tr_error.append(td_a_error.clone());
    tr_error.append('<td class="right">' + ((100.0 * error) / total).toFixed(2) + '%</td>');
    tr_error.append('<td class="right no-problem">' + error + '</td>');

    var td_a_fatal = $("tr.corpus-report-fatal").find('>:first-child');
    var tr_fatal = $('<tr class="danger corpus-report-fatal" />');
    tr_fatal.append(td_a_fatal.clone());
    tr_fatal.append('<td class="right">' + ((100.0 * fatal) / total).toFixed(2) + '%</td>');
    tr_fatal.append('<td class="right no-problem">' + fatal + '</td>');

    var table = $('<table class="table"/>');
    table.append('<thead><tr><th class="left">Status</th><th class="right">Percent</th><th class="right">Tasks</th></tr></thead>');
    var tbody = $("<tbody />");
    tbody.append(tr_no_problem);
    tbody.append(tr_warning);
    tbody.append(tr_error);
    tbody.append(tr_fatal);
    table.append(tbody);
    report_div.prepend(table);
    report_div.prepend("<h2>Rerun Progress</h2>");
  }
})