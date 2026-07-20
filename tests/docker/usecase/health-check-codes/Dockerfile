FROM sysg-usecase-base

COPY tests/docker/usecase/health-check-codes/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
