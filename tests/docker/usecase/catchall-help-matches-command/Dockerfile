FROM sysg-usecase-base

COPY tests/docker/usecase/catchall-help-matches-command/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
