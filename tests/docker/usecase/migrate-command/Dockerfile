FROM sysg-usecase-base

COPY tests/docker/usecase/migrate-command/legacy.yaml /usecase/legacy.yaml
COPY tests/docker/usecase/migrate-command/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
